//! Batch handle management via an arena that tracks pointers and their destructors.

use std::ffi::c_void;

/// Collects handles and their destructors so they can be freed in one batch.
pub struct HandleArena {
    handles: Vec<*mut c_void>,
    destructors: Vec<unsafe extern "C" fn(*mut c_void)>,
}

impl HandleArena {
    pub fn new() -> Self {
        Self {
            handles: Vec::new(),
            destructors: Vec::new(),
        }
    }

    /// Register a handle and its destructor for later batch destruction.
    pub fn register(&mut self, ptr: *mut c_void, dtor: unsafe extern "C" fn(*mut c_void)) {
        self.handles.push(ptr);
        self.destructors.push(dtor);
    }

    /// Call every registered destructor and clear the arena.
    pub fn destroy_all(&mut self) {
        for (ptr, dtor) in self.handles.drain(..).zip(self.destructors.drain(..)) {
            unsafe { dtor(ptr) };
        }
    }
}

impl Default for HandleArena {
    fn default() -> Self {
        Self::new()
    }
}

/// Create a new arena, returning an owned pointer. Free with `weaveffi_arena_destroy`.
#[no_mangle]
pub extern "C" fn weaveffi_arena_create() -> *mut HandleArena {
    Box::into_raw(Box::new(HandleArena::new()))
}

/// Register a handle and its destructor with the given arena.
///
/// # Safety
///
/// `arena` must be a valid pointer returned by `weaveffi_arena_create`.
/// `ptr` and `dtor` must remain valid until `weaveffi_arena_destroy` is called.
#[no_mangle]
pub extern "C" fn weaveffi_arena_register(
    arena: *mut HandleArena,
    ptr: *mut c_void,
    dtor: unsafe extern "C" fn(*mut c_void),
) {
    if arena.is_null() {
        return;
    }
    let arena = unsafe { &mut *arena };
    arena.register(ptr, dtor);
}

/// Destroy all handles in the arena, then free the arena itself.
///
/// # Safety
///
/// `arena` must be a valid pointer returned by `weaveffi_arena_create` and must
/// not be used after this call.
#[no_mangle]
pub extern "C" fn weaveffi_arena_destroy(arena: *mut HandleArena) {
    if arena.is_null() {
        return;
    }
    let mut arena = unsafe { *Box::from_raw(arena) };
    arena.destroy_all();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static DESTROY_COUNT: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn test_dtor(_ptr: *mut c_void) {
        DESTROY_COUNT.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn arena_destroys_all_handles() {
        DESTROY_COUNT.store(0, Ordering::SeqCst);

        let mut arena = HandleArena::new();
        for i in 1..=5 {
            arena.register(i as *mut c_void, test_dtor);
        }

        assert_eq!(DESTROY_COUNT.load(Ordering::SeqCst), 0);
        arena.destroy_all();
        assert_eq!(DESTROY_COUNT.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn arena_destroy_all_is_idempotent() {
        DESTROY_COUNT.store(0, Ordering::SeqCst);

        let mut arena = HandleArena::new();
        arena.register(1 as *mut c_void, test_dtor);
        arena.destroy_all();
        arena.destroy_all();
        assert_eq!(DESTROY_COUNT.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn arena_c_api_roundtrip() {
        DESTROY_COUNT.store(0, Ordering::SeqCst);

        let arena = weaveffi_arena_create();
        assert!(!arena.is_null());

        weaveffi_arena_register(arena, 1 as *mut c_void, test_dtor);
        weaveffi_arena_register(arena, 2 as *mut c_void, test_dtor);

        weaveffi_arena_destroy(arena);
        assert_eq!(DESTROY_COUNT.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn arena_null_register_is_safe() {
        weaveffi_arena_register(std::ptr::null_mut(), 1 as *mut c_void, test_dtor);
    }

    #[test]
    fn arena_null_destroy_is_safe() {
        weaveffi_arena_destroy(std::ptr::null_mut());
    }
}
