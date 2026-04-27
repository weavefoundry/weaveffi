// Smoke test for the Android consumer.
//
// Verifies that JNI binding declarations matching the calculator and
// contacts C ABI compile under kotlinc. We intentionally avoid loading
// the native library because CI does not run on-device; this file is
// only consumed by `kotlinc -d` during the end-to-end tests.
package com.weaveffi.example

object Calculator {
    init { System.loadLibrary("calculator") }

    @JvmStatic external fun weaveffi_calculator_add(a: Int, b: Int): Int
}

object Contacts {
    init { System.loadLibrary("contacts") }

    @JvmStatic external fun weaveffi_contacts_create_contact(
        firstName: String,
        lastName: String,
        email: String?,
        contactType: Int,
    ): Long

    @JvmStatic external fun weaveffi_contacts_count_contacts(): Int

    @JvmStatic external fun weaveffi_contacts_delete_contact(id: Long): Int
}

fun main() {
    println("OK")
}
