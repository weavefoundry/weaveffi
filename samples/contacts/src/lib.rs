#![allow(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::os::raw::c_char;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;
use weaveffi_abi::{self as abi, weaveffi_error, weaveffi_handle_t};

// --- Enum: ContactType ---

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactType {
    Personal = 0,
    Work = 1,
    Other = 2,
}

impl ContactType {
    fn from_i32(v: i32) -> Option<Self> {
        match v {
            0 => Some(Self::Personal),
            1 => Some(Self::Work),
            2 => Some(Self::Other),
            _ => None,
        }
    }
}

// --- Struct: Contact ---

#[derive(Debug, Clone)]
pub struct Contact {
    pub id: i64,
    pub first_name: String,
    pub last_name: String,
    pub email: Option<String>,
    pub contact_type: ContactType,
}

// --- In-memory store ---

static STORE: Mutex<Vec<Contact>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicI64 = AtomicI64::new(1);

// --- Module functions ---

#[no_mangle]
pub extern "C" fn weaveffi_contacts_create_contact(
    first_name: *const c_char,
    last_name: *const c_char,
    email: *const c_char,
    contact_type: i32,
    out_err: *mut weaveffi_error,
) -> weaveffi_handle_t {
    let first_name = match abi::c_ptr_to_string(first_name) {
        Some(s) => s,
        None => {
            abi::error_set(out_err, 1, "first_name is null or invalid UTF-8");
            return 0;
        }
    };
    let last_name = match abi::c_ptr_to_string(last_name) {
        Some(s) => s,
        None => {
            abi::error_set(out_err, 1, "last_name is null or invalid UTF-8");
            return 0;
        }
    };
    let email = abi::c_ptr_to_string(email);
    let ct = match ContactType::from_i32(contact_type) {
        Some(ct) => ct,
        None => {
            abi::error_set(out_err, 1, "invalid contact_type value");
            return 0;
        }
    };

    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let contact = Contact {
        id,
        first_name,
        last_name,
        email,
        contact_type: ct,
    };
    STORE.lock().unwrap().push(contact);

    abi::error_set_ok(out_err);
    id as weaveffi_handle_t
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_get_contact(
    id: weaveffi_handle_t,
    out_err: *mut weaveffi_error,
) -> *mut Contact {
    let store = STORE.lock().unwrap();
    match store.iter().find(|c| c.id == id as i64) {
        Some(c) => {
            abi::error_set_ok(out_err);
            Box::into_raw(Box::new(c.clone()))
        }
        None => {
            abi::error_set(out_err, 1, "contact not found");
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_list_contacts(
    out_len: *mut usize,
    out_err: *mut weaveffi_error,
) -> *mut *mut Contact {
    let store = STORE.lock().unwrap();
    let len = store.len();

    if !out_len.is_null() {
        unsafe { *out_len = len };
    }

    if len == 0 {
        abi::error_set_ok(out_err);
        return std::ptr::null_mut();
    }

    let mut ptrs: Vec<*mut Contact> = store
        .iter()
        .map(|c| Box::into_raw(Box::new(c.clone())))
        .collect();
    let ptr = ptrs.as_mut_ptr();
    std::mem::forget(ptrs);

    abi::error_set_ok(out_err);
    ptr
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_delete_contact(
    id: weaveffi_handle_t,
    out_err: *mut weaveffi_error,
) -> i32 {
    let mut store = STORE.lock().unwrap();
    let before = store.len();
    store.retain(|c| c.id != id as i64);
    abi::error_set_ok(out_err);
    (store.len() < before) as i32
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_count_contacts(out_err: *mut weaveffi_error) -> i32 {
    let store = STORE.lock().unwrap();
    abi::error_set_ok(out_err);
    store.len() as i32
}

// --- Contact struct getters ---

#[no_mangle]
pub extern "C" fn weaveffi_contacts_Contact_get_id(contact: *const Contact) -> i64 {
    assert!(!contact.is_null());
    unsafe { (*contact).id }
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_Contact_get_first_name(
    contact: *const Contact,
) -> *const c_char {
    assert!(!contact.is_null());
    abi::string_to_c_ptr(&unsafe { &*contact }.first_name)
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_Contact_get_last_name(
    contact: *const Contact,
) -> *const c_char {
    assert!(!contact.is_null());
    abi::string_to_c_ptr(&unsafe { &*contact }.last_name)
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_Contact_get_email(contact: *const Contact) -> *const c_char {
    assert!(!contact.is_null());
    match &unsafe { &*contact }.email {
        Some(e) => abi::string_to_c_ptr(e),
        None => std::ptr::null(),
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_Contact_get_contact_type(contact: *const Contact) -> i32 {
    assert!(!contact.is_null());
    unsafe { (*contact).contact_type as i32 }
}

// --- Contact struct setters ---

#[no_mangle]
pub extern "C" fn weaveffi_contacts_Contact_set_id(contact: *mut Contact, id: i64) {
    assert!(!contact.is_null());
    unsafe { (*contact).id = id };
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_Contact_set_first_name(
    contact: *mut Contact,
    first_name: *const c_char,
) {
    assert!(!contact.is_null());
    if let Some(s) = abi::c_ptr_to_string(first_name) {
        unsafe { (*contact).first_name = s };
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_Contact_set_last_name(
    contact: *mut Contact,
    last_name: *const c_char,
) {
    assert!(!contact.is_null());
    if let Some(s) = abi::c_ptr_to_string(last_name) {
        unsafe { (*contact).last_name = s };
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_Contact_set_email(contact: *mut Contact, email: *const c_char) {
    assert!(!contact.is_null());
    unsafe { (*contact).email = abi::c_ptr_to_string(email) };
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_Contact_set_contact_type(
    contact: *mut Contact,
    contact_type: i32,
) {
    assert!(!contact.is_null());
    if let Some(ct) = ContactType::from_i32(contact_type) {
        unsafe { (*contact).contact_type = ct };
    }
}

// --- Enum conversion functions ---

#[no_mangle]
pub extern "C" fn weaveffi_contacts_ContactType_from_i32(
    value: i32,
    out_err: *mut weaveffi_error,
) -> i32 {
    match ContactType::from_i32(value) {
        Some(ct) => {
            abi::error_set_ok(out_err);
            ct as i32
        }
        None => {
            abi::error_set(out_err, 1, "invalid ContactType value");
            -1
        }
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_ContactType_to_i32(ct: i32) -> i32 {
    ct
}

// --- Free functions ---

#[no_mangle]
pub extern "C" fn weaveffi_contacts_Contact_destroy(contact: *mut Contact) {
    if contact.is_null() {
        return;
    }
    unsafe { drop(Box::from_raw(contact)) };
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_Contact_list_free(contacts: *mut *mut Contact, len: usize) {
    if contacts.is_null() {
        return;
    }
    let ptrs = unsafe { Vec::from_raw_parts(contacts, len, len) };
    for ptr in ptrs {
        if !ptr.is_null() {
            unsafe { drop(Box::from_raw(ptr)) };
        }
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_free_string(ptr: *const c_char) {
    abi::free_string(ptr)
}

#[no_mangle]
pub extern "C" fn weaveffi_error_clear(err: *mut weaveffi_error) {
    abi::error_clear(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    fn setup() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_MUTEX.lock().unwrap();
        STORE.lock().unwrap().clear();
        NEXT_ID.store(1, Ordering::Relaxed);
        guard
    }

    fn new_err() -> weaveffi_error {
        weaveffi_error::default()
    }

    #[test]
    fn create_and_get_contact() {
        let _g = setup();
        let mut err = new_err();
        let first = CString::new("Alice").unwrap();
        let last = CString::new("Smith").unwrap();
        let email = CString::new("alice@example.com").unwrap();

        let handle = weaveffi_contacts_create_contact(
            first.as_ptr(),
            last.as_ptr(),
            email.as_ptr(),
            ContactType::Work as i32,
            &mut err,
        );
        assert_eq!(err.code, 0);
        assert!(handle > 0);

        let contact = weaveffi_contacts_get_contact(handle, &mut err);
        assert_eq!(err.code, 0);
        assert!(!contact.is_null());

        let c = unsafe { &*contact };
        assert_eq!(c.first_name, "Alice");
        assert_eq!(c.last_name, "Smith");
        assert_eq!(c.email, Some("alice@example.com".to_string()));
        assert_eq!(c.contact_type, ContactType::Work);

        weaveffi_contacts_Contact_destroy(contact);
    }

    #[test]
    fn create_contact_with_null_email() {
        let _g = setup();
        let mut err = new_err();
        let first = CString::new("Bob").unwrap();
        let last = CString::new("Jones").unwrap();

        let handle = weaveffi_contacts_create_contact(
            first.as_ptr(),
            last.as_ptr(),
            std::ptr::null(),
            ContactType::Personal as i32,
            &mut err,
        );
        assert_eq!(err.code, 0);

        let contact = weaveffi_contacts_get_contact(handle, &mut err);
        assert!(!contact.is_null());
        assert_eq!(unsafe { &*contact }.email, None);
        weaveffi_contacts_Contact_destroy(contact);
    }

    #[test]
    fn create_contact_invalid_type() {
        let _g = setup();
        let mut err = new_err();
        let first = CString::new("Bad").unwrap();
        let last = CString::new("Type").unwrap();

        let handle = weaveffi_contacts_create_contact(
            first.as_ptr(),
            last.as_ptr(),
            std::ptr::null(),
            99,
            &mut err,
        );
        assert_eq!(handle, 0);
        assert_ne!(err.code, 0);
        abi::error_clear(&mut err);
    }

    #[test]
    fn create_contact_null_first_name() {
        let _g = setup();
        let mut err = new_err();
        let last = CString::new("Oops").unwrap();

        let handle = weaveffi_contacts_create_contact(
            std::ptr::null(),
            last.as_ptr(),
            std::ptr::null(),
            0,
            &mut err,
        );
        assert_eq!(handle, 0);
        assert_ne!(err.code, 0);
        abi::error_clear(&mut err);
    }

    #[test]
    fn count_and_list_contacts() {
        let _g = setup();
        let mut err = new_err();
        assert_eq!(weaveffi_contacts_count_contacts(&mut err), 0);

        let first = CString::new("A").unwrap();
        let last = CString::new("B").unwrap();
        weaveffi_contacts_create_contact(
            first.as_ptr(),
            last.as_ptr(),
            std::ptr::null(),
            0,
            &mut err,
        );
        weaveffi_contacts_create_contact(
            first.as_ptr(),
            last.as_ptr(),
            std::ptr::null(),
            1,
            &mut err,
        );
        assert_eq!(weaveffi_contacts_count_contacts(&mut err), 2);

        let mut len: usize = 0;
        let list = weaveffi_contacts_list_contacts(&mut len, &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(len, 2);
        assert!(!list.is_null());

        weaveffi_contacts_Contact_list_free(list, len);
    }

    #[test]
    fn list_contacts_empty() {
        let _g = setup();
        let mut err = new_err();
        let mut len: usize = 999;
        let list = weaveffi_contacts_list_contacts(&mut len, &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(len, 0);
        assert!(list.is_null());
    }

    #[test]
    fn delete_contact() {
        let _g = setup();
        let mut err = new_err();
        let first = CString::new("Del").unwrap();
        let last = CString::new("Me").unwrap();
        let handle = weaveffi_contacts_create_contact(
            first.as_ptr(),
            last.as_ptr(),
            std::ptr::null(),
            0,
            &mut err,
        );
        assert_eq!(weaveffi_contacts_count_contacts(&mut err), 1);

        assert_eq!(weaveffi_contacts_delete_contact(handle, &mut err), 1);
        assert_eq!(weaveffi_contacts_count_contacts(&mut err), 0);

        assert_eq!(weaveffi_contacts_delete_contact(handle, &mut err), 0);
    }

    #[test]
    fn get_contact_not_found() {
        let _g = setup();
        let mut err = new_err();
        let contact = weaveffi_contacts_get_contact(999, &mut err);
        assert!(contact.is_null());
        assert_ne!(err.code, 0);
        abi::error_clear(&mut err);
    }

    #[test]
    fn contact_getters_and_setters() {
        let _g = setup();
        let mut err = new_err();
        let first = CString::new("Get").unwrap();
        let last = CString::new("Set").unwrap();
        let email = CString::new("gs@test.com").unwrap();

        let handle = weaveffi_contacts_create_contact(
            first.as_ptr(),
            last.as_ptr(),
            email.as_ptr(),
            0,
            &mut err,
        );
        let contact = weaveffi_contacts_get_contact(handle, &mut err);
        assert!(!contact.is_null());

        assert_eq!(weaveffi_contacts_Contact_get_id(contact), handle as i64);
        assert_eq!(weaveffi_contacts_Contact_get_contact_type(contact), 0);

        let fname = weaveffi_contacts_Contact_get_first_name(contact);
        assert_eq!(abi::c_ptr_to_string(fname).unwrap(), "Get");
        abi::free_string(fname);

        let lname = weaveffi_contacts_Contact_get_last_name(contact);
        assert_eq!(abi::c_ptr_to_string(lname).unwrap(), "Set");
        abi::free_string(lname);

        let em = weaveffi_contacts_Contact_get_email(contact);
        assert_eq!(abi::c_ptr_to_string(em).unwrap(), "gs@test.com");
        abi::free_string(em);

        // Setters
        let new_first = CString::new("Updated").unwrap();
        weaveffi_contacts_Contact_set_first_name(contact, new_first.as_ptr());
        let fname2 = weaveffi_contacts_Contact_get_first_name(contact);
        assert_eq!(abi::c_ptr_to_string(fname2).unwrap(), "Updated");
        abi::free_string(fname2);

        weaveffi_contacts_Contact_set_id(contact, 42);
        assert_eq!(weaveffi_contacts_Contact_get_id(contact), 42);

        weaveffi_contacts_Contact_set_contact_type(contact, 2);
        assert_eq!(weaveffi_contacts_Contact_get_contact_type(contact), 2);

        weaveffi_contacts_Contact_set_email(contact, std::ptr::null());
        assert!(weaveffi_contacts_Contact_get_email(contact).is_null());

        weaveffi_contacts_Contact_destroy(contact);
    }

    #[test]
    fn contact_type_conversions() {
        let mut err = new_err();
        assert_eq!(weaveffi_contacts_ContactType_from_i32(0, &mut err), 0);
        assert_eq!(err.code, 0);
        assert_eq!(weaveffi_contacts_ContactType_from_i32(1, &mut err), 1);
        assert_eq!(weaveffi_contacts_ContactType_from_i32(2, &mut err), 2);

        assert_eq!(weaveffi_contacts_ContactType_from_i32(99, &mut err), -1);
        assert_ne!(err.code, 0);
        abi::error_clear(&mut err);

        assert_eq!(weaveffi_contacts_ContactType_to_i32(0), 0);
        assert_eq!(weaveffi_contacts_ContactType_to_i32(1), 1);
        assert_eq!(weaveffi_contacts_ContactType_to_i32(2), 2);
    }

    #[test]
    fn free_null_contact_is_safe() {
        weaveffi_contacts_Contact_destroy(std::ptr::null_mut());
    }

    #[test]
    fn free_null_contact_list_is_safe() {
        weaveffi_contacts_Contact_list_free(std::ptr::null_mut(), 0);
    }
}
