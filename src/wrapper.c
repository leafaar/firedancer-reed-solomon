/* Wrapper functions to expose Firedancer's inline Reed-Solomon functions to Rust FFI */

#include "../firedancer/src/ballet/reedsol/fd_reedsol.h"

/* Re-export the non-inline functions */
/* fd_reedsol_encode_fini and fd_reedsol_recover_fini are already non-inline in fd_reedsol.c */

/* Wrapper for inline fd_reedsol_align */
unsigned long fd_reedsol_align_wrapper(void) {
    return fd_reedsol_align();
}

/* Wrapper for inline fd_reedsol_footprint */
unsigned long fd_reedsol_footprint_wrapper(void) {
    return fd_reedsol_footprint();
}

/* Wrapper for inline fd_reedsol_encode_init */
fd_reedsol_t * fd_reedsol_encode_init_wrapper(void * mem, unsigned long shred_sz) {
    return fd_reedsol_encode_init(mem, shred_sz);
}

/* Wrapper for inline fd_reedsol_encode_add_data_shred */
fd_reedsol_t * fd_reedsol_encode_add_data_shred_wrapper(fd_reedsol_t * rs, void const * ptr) {
    return fd_reedsol_encode_add_data_shred(rs, ptr);
}

/* Wrapper for inline fd_reedsol_encode_add_parity_shred */
fd_reedsol_t * fd_reedsol_encode_add_parity_shred_wrapper(fd_reedsol_t * rs, void * ptr) {
    return fd_reedsol_encode_add_parity_shred(rs, ptr);
}

/* Wrapper for inline fd_reedsol_encode_abort */
void fd_reedsol_encode_abort_wrapper(fd_reedsol_t * rs) {
    fd_reedsol_encode_abort(rs);
}

/* Wrapper for inline fd_reedsol_recover_init */
fd_reedsol_t * fd_reedsol_recover_init_wrapper(void * mem, unsigned long shred_sz) {
    return fd_reedsol_recover_init(mem, shred_sz);
}

/* Wrapper for inline fd_reedsol_recover_add_rcvd_shred */
fd_reedsol_t * fd_reedsol_recover_add_rcvd_shred_wrapper(fd_reedsol_t * rs, int is_data_shred, void const * ptr) {
    return fd_reedsol_recover_add_rcvd_shred(rs, is_data_shred, ptr);
}

/* Wrapper for inline fd_reedsol_recover_add_erased_shred */
fd_reedsol_t * fd_reedsol_recover_add_erased_shred_wrapper(fd_reedsol_t * rs, int is_data_shred, void * ptr) {
    return fd_reedsol_recover_add_erased_shred(rs, is_data_shred, ptr);
}

/* Wrapper for inline fd_reedsol_recover_abort */
void fd_reedsol_recover_abort_wrapper(fd_reedsol_t * rs) {
    fd_reedsol_recover_abort(rs);
}
