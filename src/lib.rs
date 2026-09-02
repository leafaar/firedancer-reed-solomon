//! Firedancer Reed-Solomon erasure coding
//!
//! This crate provides high-performance Reed-Solomon erasure coding using
//! Firedancer's optimized implementation with AVX2/GFNI SIMD acceleration.
//!
//! The API is designed to be compatible with `reed-solomon-erasure` for easy
//! integration with existing Solana/Agave codebases.

use std::alloc::{alloc, dealloc, Layout};
use std::ptr::NonNull;
use thiserror::Error;

// FFI declarations for the Firedancer Reed-Solomon functions
#[allow(dead_code)]
mod ffi {
    use std::ffi::c_void;

    // Opaque type for fd_reedsol_t
    #[repr(C)]
    pub struct FdReedsol {
        _data: [u8; 0],
        _marker: std::marker::PhantomData<(*mut u8, std::marker::PhantomPinned)>,
    }

    extern "C" {
        // Utility functions
        pub fn fd_reedsol_align_wrapper() -> usize;
        pub fn fd_reedsol_footprint_wrapper() -> usize;
        pub fn fd_reedsol_strerror(err: i32) -> *const std::ffi::c_char;

        // Encode functions
        pub fn fd_reedsol_encode_init_wrapper(mem: *mut c_void, shred_sz: usize) -> *mut FdReedsol;
        pub fn fd_reedsol_encode_add_data_shred_wrapper(
            rs: *mut FdReedsol,
            ptr: *const c_void,
        ) -> *mut FdReedsol;
        pub fn fd_reedsol_encode_add_parity_shred_wrapper(
            rs: *mut FdReedsol,
            ptr: *mut c_void,
        ) -> *mut FdReedsol;
        pub fn fd_reedsol_encode_fini(rs: *mut FdReedsol);
        pub fn fd_reedsol_encode_abort_wrapper(rs: *mut FdReedsol);

        // Recover functions
        pub fn fd_reedsol_recover_init_wrapper(mem: *mut c_void, shred_sz: usize)
            -> *mut FdReedsol;
        pub fn fd_reedsol_recover_add_rcvd_shred_wrapper(
            rs: *mut FdReedsol,
            is_data_shred: i32,
            ptr: *const c_void,
        ) -> *mut FdReedsol;
        pub fn fd_reedsol_recover_add_erased_shred_wrapper(
            rs: *mut FdReedsol,
            is_data_shred: i32,
            ptr: *mut c_void,
        ) -> *mut FdReedsol;
        pub fn fd_reedsol_recover_fini(rs: *mut FdReedsol) -> i32;
        pub fn fd_reedsol_recover_abort_wrapper(rs: *mut FdReedsol);
    }
}

/// Maximum number of data shreds supported
pub const DATA_SHREDS_MAX: usize = 67;

/// Maximum number of parity shreds supported
pub const PARITY_SHREDS_MAX: usize = 67;

/// Minimum shred size in bytes
pub const SHRED_SIZE_MIN: usize = 32;

/// Success error code
const SUCCESS: i32 = 0;

/// Corrupt data error code
const ERR_CORRUPT: i32 = -1;

/// Partial data error code (not enough shreds to recover)
const ERR_PARTIAL: i32 = -2;

/// Errors that can occur during Reed-Solomon operations
#[derive(Debug, Clone, Error)]
pub enum Error {
    /// Invalid configuration (e.g., too many shreds, shred size too small)
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    /// Not enough shreds to recover data
    #[error("not enough shreds to recover data (need at least {data_shred_cnt} unerased shreds)")]
    TooFewShards { data_shred_cnt: usize },

    /// Data is corrupted
    #[error("shreds are corrupted or inconsistent")]
    Corrupt,

    /// Shred size mismatch
    #[error("shred size mismatch")]
    ShredSizeMismatch,
}

/// Reed-Solomon encoder/decoder using Firedancer's optimized implementation
///
/// This struct provides a compatible API with `reed-solomon-erasure::ReedSolomon`
/// for easy integration.
pub struct ReedSolomon {
    data_shard_count: usize,
    parity_shard_count: usize,
    // Memory for the fd_reedsol_t structure
    mem: NonNull<u8>,
    layout: Layout,
}

// Safety: the workspace pointer behind `mem` is owned by this value and only
// freed in Drop, so moving the value to another thread is fine.
unsafe impl Send for ReedSolomon {}

// Deliberately not Sync: every encode/reconstruct scribbles over the shared
// fd_reedsol_t workspace behind `mem` even though it used to take &self, so
// concurrent calls on one instance would race. Use one instance per thread.

impl ReedSolomon {
    /// Creates a new Reed-Solomon encoder/decoder
    ///
    /// # Arguments
    /// * `data_shard_count` - Number of data shards (1 to 67)
    /// * `parity_shard_count` - Number of parity shards (1 to 67)
    ///
    /// # Errors
    /// Returns an error if the shard counts are invalid
    pub fn new(data_shard_count: usize, parity_shard_count: usize) -> Result<Self, Error> {
        if data_shard_count == 0 || data_shard_count > DATA_SHREDS_MAX {
            return Err(Error::InvalidConfig(format!(
                "data_shard_count must be between 1 and {}, got {}",
                DATA_SHREDS_MAX, data_shard_count
            )));
        }

        if parity_shard_count == 0 || parity_shard_count > PARITY_SHREDS_MAX {
            return Err(Error::InvalidConfig(format!(
                "parity_shard_count must be between 1 and {}, got {}",
                PARITY_SHREDS_MAX, parity_shard_count
            )));
        }

        if data_shard_count + parity_shard_count > DATA_SHREDS_MAX + PARITY_SHREDS_MAX {
            return Err(Error::InvalidConfig(format!(
                "total shards must not exceed {}, got {}",
                DATA_SHREDS_MAX + PARITY_SHREDS_MAX,
                data_shard_count + parity_shard_count
            )));
        }

        // Allocate memory for fd_reedsol_t
        let align = unsafe { ffi::fd_reedsol_align_wrapper() };
        let footprint = unsafe { ffi::fd_reedsol_footprint_wrapper() };
        let layout = Layout::from_size_align(footprint, align)
            .map_err(|e| Error::InvalidConfig(format!("invalid layout: {}", e)))?;

        let mem = unsafe {
            let ptr = alloc(layout);
            if ptr.is_null() {
                return Err(Error::InvalidConfig(
                    "failed to allocate memory".to_string(),
                ));
            }
            NonNull::new_unchecked(ptr)
        };

        Ok(Self {
            data_shard_count,
            parity_shard_count,
            mem,
            layout,
        })
    }

    /// Returns the number of data shards
    pub fn data_shard_count(&self) -> usize {
        self.data_shard_count
    }

    /// Returns the number of parity shards
    pub fn parity_shard_count(&self) -> usize {
        self.parity_shard_count
    }

    /// Returns the total number of shards (data + parity)
    pub fn total_shard_count(&self) -> usize {
        self.data_shard_count + self.parity_shard_count
    }

    /// Encodes data shards and produces parity shards
    ///
    /// # Arguments
    /// * `shards` - Slice of shards where the first `data_shard_count` are data shards
    ///              and the remaining are parity shards to be filled
    ///
    /// # Safety
    /// All shards must have the same length (at least 32 bytes)
    pub fn encode<T: AsRef<[u8]> + AsMut<[u8]>>(&mut self, shards: &mut [T]) -> Result<(), Error> {
        if shards.len() != self.total_shard_count() {
            return Err(Error::InvalidConfig(format!(
                "expected {} shards, got {}",
                self.total_shard_count(),
                shards.len()
            )));
        }

        if shards.is_empty() {
            return Ok(());
        }

        let shred_sz = shards[0].as_ref().len();
        if shred_sz < SHRED_SIZE_MIN {
            return Err(Error::InvalidConfig(format!(
                "shred size must be at least {}, got {}",
                SHRED_SIZE_MIN, shred_sz
            )));
        }

        // Verify all shards have the same size
        for shard in shards.iter() {
            if shard.as_ref().len() != shred_sz {
                return Err(Error::ShredSizeMismatch);
            }
        }

        unsafe {
            let rs = ffi::fd_reedsol_encode_init_wrapper(
                self.mem.as_ptr() as *mut std::ffi::c_void,
                shred_sz,
            );
            if rs.is_null() {
                return Err(Error::InvalidConfig(
                    "failed to initialize encoder".to_string(),
                ));
            }

            // Add data shreds
            for i in 0..self.data_shard_count {
                ffi::fd_reedsol_encode_add_data_shred_wrapper(
                    rs,
                    shards[i].as_ref().as_ptr() as *const std::ffi::c_void,
                );
            }

            // Add parity shreds
            for i in self.data_shard_count..self.total_shard_count() {
                ffi::fd_reedsol_encode_add_parity_shred_wrapper(
                    rs,
                    shards[i].as_mut().as_mut_ptr() as *mut std::ffi::c_void,
                );
            }

            // Finalize encoding
            ffi::fd_reedsol_encode_fini(rs);
        }

        Ok(())
    }

    /// Reconstructs missing shards from available data and parity shards
    ///
    /// # Arguments
    /// * `shards` - Slice of `Option<T>` where `None` indicates a missing shard
    ///
    /// The first `data_shard_count` elements are data shards, the rest are parity.
    /// Missing shards (`None`) will be reconstructed and replaced with `Some(...)`.
    ///
    /// # Errors
    /// Returns an error if there aren't enough shards to reconstruct the data
    pub fn reconstruct<T: AsRef<[u8]> + AsMut<[u8]> + Default>(
        &mut self,
        shards: &mut [Option<T>],
    ) -> Result<(), Error> {
        if shards.len() != self.total_shard_count() {
            return Err(Error::InvalidConfig(format!(
                "expected {} shards, got {}",
                self.total_shard_count(),
                shards.len()
            )));
        }

        // Find the shred size from a non-None shard
        let shred_sz = shards
            .iter()
            .find_map(|s| s.as_ref().map(|s| s.as_ref().len()))
            .ok_or_else(|| Error::TooFewShards {
                data_shred_cnt: self.data_shard_count,
            })?;

        if shred_sz < SHRED_SIZE_MIN {
            return Err(Error::InvalidConfig(format!(
                "shred size must be at least {}, got {}",
                SHRED_SIZE_MIN, shred_sz
            )));
        }

        // Count missing shards
        let missing_count = shards.iter().filter(|s| s.is_none()).count();
        if missing_count == 0 {
            return Ok(()); // Nothing to reconstruct
        }

        // Check we have enough shards
        let available = self.total_shard_count() - missing_count;
        if available < self.data_shard_count {
            return Err(Error::TooFewShards {
                data_shred_cnt: self.data_shard_count,
            });
        }

        // Allocate buffers for missing shards
        for shard in shards.iter_mut() {
            if shard.is_none() {
                let buf = T::default();
                // Ensure the buffer is the right size
                // This assumes T::default() creates an appropriately sized buffer
                // For Vec<u8>, you'd need a custom implementation
                *shard = Some(buf);
            }
        }

        unsafe {
            let rs = ffi::fd_reedsol_recover_init_wrapper(
                self.mem.as_ptr() as *mut std::ffi::c_void,
                shred_sz,
            );
            if rs.is_null() {
                return Err(Error::InvalidConfig(
                    "failed to initialize recovery".to_string(),
                ));
            }

            // Add all shards in order
            // Data shreds first, then parity shreds
            for i in 0..self.data_shard_count {
                if let Some(ref shard) = shards[i] {
                    ffi::fd_reedsol_recover_add_rcvd_shred_wrapper(
                        rs,
                        1, // is_data_shred = true
                        shard.as_ref().as_ptr() as *const std::ffi::c_void,
                    );
                } else {
                    ffi::fd_reedsol_recover_add_erased_shred_wrapper(
                        rs,
                        1, // is_data_shred = true
                        shards[i].as_mut().unwrap().as_mut().as_mut_ptr() as *mut std::ffi::c_void,
                    );
                }
            }

            for i in self.data_shard_count..self.total_shard_count() {
                if let Some(ref shard) = shards[i] {
                    ffi::fd_reedsol_recover_add_rcvd_shred_wrapper(
                        rs,
                        0, // is_data_shred = false
                        shard.as_ref().as_ptr() as *const std::ffi::c_void,
                    );
                } else {
                    ffi::fd_reedsol_recover_add_erased_shred_wrapper(
                        rs,
                        0, // is_data_shred = false
                        shards[i].as_mut().unwrap().as_mut().as_mut_ptr() as *mut std::ffi::c_void,
                    );
                }
            }

            // Finalize recovery
            let result = ffi::fd_reedsol_recover_fini(rs);
            match result {
                SUCCESS => Ok(()),
                ERR_CORRUPT => Err(Error::Corrupt),
                ERR_PARTIAL => Err(Error::TooFewShards {
                    data_shred_cnt: self.data_shard_count,
                }),
                _ => Err(Error::InvalidConfig(format!("unknown error: {}", result))),
            }
        }
    }

    /// Reconstructs missing shards in-place using (data, is_present) tuples
    ///
    /// This method provides API compatibility with `reed-solomon-erasure::ReedSolomon::reconstruct`.
    ///
    /// # Arguments
    /// * `shards` - Mutable slice of `(T, bool)` tuples where `T` implements `AsMut<[u8]>` and `AsRef<[u8]>`.
    ///              The boolean indicates whether the shard is present (true) or missing (false).
    pub fn reconstruct_with_mask<T: AsMut<[u8]> + AsRef<[u8]>>(
        &mut self,
        shards: &mut [(T, bool)],
    ) -> Result<(), Error> {
        if shards.len() != self.total_shard_count() {
            return Err(Error::InvalidConfig(format!(
                "expected {} shards, got {}",
                self.total_shard_count(),
                shards.len()
            )));
        }

        if shards.is_empty() {
            return Ok(());
        }

        let shred_sz = shards[0].0.as_ref().len();
        if shred_sz < SHRED_SIZE_MIN {
            return Err(Error::InvalidConfig(format!(
                "shred size must be at least {}, got {}",
                SHRED_SIZE_MIN, shred_sz
            )));
        }

        // Verify all shards have the same size
        for (shard, _) in shards.iter() {
            if shard.as_ref().len() != shred_sz {
                return Err(Error::ShredSizeMismatch);
            }
        }

        // Count available shards
        let available = shards.iter().filter(|(_, present)| *present).count();
        if available < self.data_shard_count {
            return Err(Error::TooFewShards {
                data_shred_cnt: self.data_shard_count,
            });
        }

        // Check if all shards are present
        if available == self.total_shard_count() {
            return Ok(()); // Nothing to reconstruct
        }

        unsafe {
            let rs = ffi::fd_reedsol_recover_init_wrapper(
                self.mem.as_ptr() as *mut std::ffi::c_void,
                shred_sz,
            );
            if rs.is_null() {
                return Err(Error::InvalidConfig(
                    "failed to initialize recovery".to_string(),
                ));
            }

            // Add data shreds
            for i in 0..self.data_shard_count {
                let (shard, present) = &mut shards[i];
                if *present {
                    ffi::fd_reedsol_recover_add_rcvd_shred_wrapper(
                        rs,
                        1,
                        shard.as_ref().as_ptr() as *const std::ffi::c_void,
                    );
                } else {
                    ffi::fd_reedsol_recover_add_erased_shred_wrapper(
                        rs,
                        1,
                        shard.as_mut().as_mut_ptr() as *mut std::ffi::c_void,
                    );
                }
            }

            // Add parity shreds
            for i in self.data_shard_count..self.total_shard_count() {
                let (shard, present) = &mut shards[i];
                if *present {
                    ffi::fd_reedsol_recover_add_rcvd_shred_wrapper(
                        rs,
                        0,
                        shard.as_ref().as_ptr() as *const std::ffi::c_void,
                    );
                } else {
                    ffi::fd_reedsol_recover_add_erased_shred_wrapper(
                        rs,
                        0,
                        shard.as_mut().as_mut_ptr() as *mut std::ffi::c_void,
                    );
                }
            }

            let result = ffi::fd_reedsol_recover_fini(rs);
            match result {
                SUCCESS => Ok(()),
                ERR_CORRUPT => Err(Error::Corrupt),
                ERR_PARTIAL => Err(Error::TooFewShards {
                    data_shred_cnt: self.data_shard_count,
                }),
                _ => Err(Error::InvalidConfig(format!("unknown error: {}", result))),
            }
        }
    }

    /// Reconstructs missing shards in-place using a slice of (data, is_present) tuples
    ///
    /// This method matches the `reed-solomon-erasure` API more closely.
    ///
    /// # Arguments
    /// * `shards` - Mutable slice of mutable byte slices. All must be the same length.
    /// * `shard_present` - Slice indicating which shards are present (true) or missing (false)
    pub fn reconstruct_shards(
        &mut self,
        shards: &mut [&mut [u8]],
        shard_present: &[bool],
    ) -> Result<(), Error> {
        if shards.len() != self.total_shard_count() {
            return Err(Error::InvalidConfig(format!(
                "expected {} shards, got {}",
                self.total_shard_count(),
                shards.len()
            )));
        }

        if shard_present.len() != shards.len() {
            return Err(Error::InvalidConfig(
                "shard_present length must match shards length".to_string(),
            ));
        }

        if shards.is_empty() {
            return Ok(());
        }

        let shred_sz = shards[0].len();
        if shred_sz < SHRED_SIZE_MIN {
            return Err(Error::InvalidConfig(format!(
                "shred size must be at least {}, got {}",
                SHRED_SIZE_MIN, shred_sz
            )));
        }

        // Verify all shards have the same size
        for shard in shards.iter() {
            if shard.len() != shred_sz {
                return Err(Error::ShredSizeMismatch);
            }
        }

        // Count available shards
        let available = shard_present.iter().filter(|&&p| p).count();
        if available < self.data_shard_count {
            return Err(Error::TooFewShards {
                data_shred_cnt: self.data_shard_count,
            });
        }

        // Check if all shards are present
        if available == self.total_shard_count() {
            return Ok(()); // Nothing to reconstruct
        }

        unsafe {
            let rs = ffi::fd_reedsol_recover_init_wrapper(
                self.mem.as_ptr() as *mut std::ffi::c_void,
                shred_sz,
            );
            if rs.is_null() {
                return Err(Error::InvalidConfig(
                    "failed to initialize recovery".to_string(),
                ));
            }

            // Add data shreds
            for i in 0..self.data_shard_count {
                if shard_present[i] {
                    ffi::fd_reedsol_recover_add_rcvd_shred_wrapper(
                        rs,
                        1,
                        shards[i].as_ptr() as *const std::ffi::c_void,
                    );
                } else {
                    ffi::fd_reedsol_recover_add_erased_shred_wrapper(
                        rs,
                        1,
                        shards[i].as_mut_ptr() as *mut std::ffi::c_void,
                    );
                }
            }

            // Add parity shreds
            for i in self.data_shard_count..self.total_shard_count() {
                if shard_present[i] {
                    ffi::fd_reedsol_recover_add_rcvd_shred_wrapper(
                        rs,
                        0,
                        shards[i].as_ptr() as *const std::ffi::c_void,
                    );
                } else {
                    ffi::fd_reedsol_recover_add_erased_shred_wrapper(
                        rs,
                        0,
                        shards[i].as_mut_ptr() as *mut std::ffi::c_void,
                    );
                }
            }

            let result = ffi::fd_reedsol_recover_fini(rs);
            match result {
                SUCCESS => Ok(()),
                ERR_CORRUPT => Err(Error::Corrupt),
                ERR_PARTIAL => Err(Error::TooFewShards {
                    data_shred_cnt: self.data_shard_count,
                }),
                _ => Err(Error::InvalidConfig(format!("unknown error: {}", result))),
            }
        }
    }
}

impl Drop for ReedSolomon {
    fn drop(&mut self) {
        unsafe {
            dealloc(self.mem.as_ptr(), self.layout);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_valid() {
        let mut rs = ReedSolomon::new(32, 32).unwrap();
        assert_eq!(rs.data_shard_count(), 32);
        assert_eq!(rs.parity_shard_count(), 32);
        assert_eq!(rs.total_shard_count(), 64);
    }

    #[test]
    fn test_new_invalid_data_count() {
        assert!(ReedSolomon::new(0, 32).is_err());
        assert!(ReedSolomon::new(68, 32).is_err());
    }

    #[test]
    fn test_new_invalid_parity_count() {
        assert!(ReedSolomon::new(32, 0).is_err());
        assert!(ReedSolomon::new(32, 68).is_err());
    }

    #[test]
    fn test_encode_decode_basic() {
        let mut rs = ReedSolomon::new(4, 2).unwrap();
        let shred_size = 64;

        // Create shards
        let mut shards: Vec<Vec<u8>> = Vec::with_capacity(6);
        for i in 0..4 {
            let mut shard = vec![0u8; shred_size];
            for j in 0..shred_size {
                shard[j] = ((i * shred_size + j) % 256) as u8;
            }
            shards.push(shard);
        }
        // Add empty parity shards
        for _ in 0..2 {
            shards.push(vec![0u8; shred_size]);
        }

        // Encode
        rs.encode(&mut shards).unwrap();

        // Verify parity shards are not all zeros
        assert!(shards[4].iter().any(|&b| b != 0) || shards[5].iter().any(|&b| b != 0));

        // Test reconstruction: simulate losing first data shard
        let original_shard0 = shards[0].clone();
        let present = [false, true, true, true, true, true];
        shards[0] = vec![0u8; shred_size]; // Clear shard 0

        let mut shard_refs: Vec<&mut [u8]> = shards.iter_mut().map(|s| s.as_mut_slice()).collect();
        rs.reconstruct_shards(&mut shard_refs, &present).unwrap();

        // Verify reconstruction
        assert_eq!(shards[0], original_shard0);
    }

    #[test]
    fn test_encode_32x32() {
        // Test the common Solana case of 32 data + 32 parity
        let mut rs = ReedSolomon::new(32, 32).unwrap();
        let shred_size = 1228; // Typical Solana shred payload size

        let mut shards: Vec<Vec<u8>> = Vec::with_capacity(64);
        for i in 0..32 {
            let mut shard = vec![0u8; shred_size];
            for j in 0..shred_size {
                shard[j] = ((i * 7 + j * 13) % 256) as u8;
            }
            shards.push(shard);
        }
        for _ in 0..32 {
            shards.push(vec![0u8; shred_size]);
        }

        rs.encode(&mut shards).unwrap();

        // Simulate losing multiple shards
        let mut present = [true; 64];
        let lost_indices = [0, 5, 10, 15, 20, 25, 30, 35, 40, 45];
        let mut originals: Vec<(usize, Vec<u8>)> = Vec::new();

        for &idx in &lost_indices {
            originals.push((idx, shards[idx].clone()));
            shards[idx] = vec![0u8; shred_size];
            present[idx] = false;
        }

        let mut shard_refs: Vec<&mut [u8]> = shards.iter_mut().map(|s| s.as_mut_slice()).collect();
        rs.reconstruct_shards(&mut shard_refs, &present).unwrap();

        // Verify all reconstructed shards
        for (idx, original) in originals {
            assert_eq!(shards[idx], original, "Shard {} mismatch", idx);
        }
    }

    #[test]
    fn test_too_few_shards() {
        let mut rs = ReedSolomon::new(4, 2).unwrap();
        let shred_size = 64;

        let mut shards: Vec<Vec<u8>> = (0..6).map(|_| vec![0u8; shred_size]).collect();
        rs.encode(&mut shards).unwrap();

        // Try to reconstruct with only 3 shards available (need 4)
        let present = [true, true, true, false, false, false];
        let mut shard_refs: Vec<&mut [u8]> = shards.iter_mut().map(|s| s.as_mut_slice()).collect();

        let result = rs.reconstruct_shards(&mut shard_refs, &present);
        assert!(matches!(result, Err(Error::TooFewShards { .. })));
    }
}
