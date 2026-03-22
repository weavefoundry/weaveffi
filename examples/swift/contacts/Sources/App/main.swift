import CWeaveFFI

@inline(__always)
func run() {
    var err = weaveffi_error(code: 0, message: nil)

    let h1 = weaveffi_contacts_create_contact("Alice", "Smith", "alice@example.com", 0, &err)
    if err.code != 0 { let msg = err.message.flatMap { String(cString: $0) } ?? ""; weaveffi_error_clear(&err); fatalError(msg) }
    print("Created contact #\(h1)")

    let h2 = weaveffi_contacts_create_contact("Bob", "Jones", nil, 1, &err)
    if err.code != 0 { let msg = err.message.flatMap { String(cString: $0) } ?? ""; weaveffi_error_clear(&err); fatalError(msg) }
    print("Created contact #\(h2)")

    let count = weaveffi_contacts_count_contacts(&err)
    if err.code != 0 { let msg = err.message.flatMap { String(cString: $0) } ?? ""; weaveffi_error_clear(&err); fatalError(msg) }
    print("\nTotal: \(count) contacts\n")

    var len: Int = 0
    let list = weaveffi_contacts_list_contacts(&len, &err)
    if err.code != 0 { let msg = err.message.flatMap { String(cString: $0) } ?? ""; weaveffi_error_clear(&err); fatalError(msg) }

    if let list = list {
        for i in 0..<len {
            guard let contact = list[i] else { continue }
            let id = weaveffi_contacts_Contact_get_id(contact)

            var line = "  [\(id)]"
            if let f = weaveffi_contacts_Contact_get_first_name(contact) {
                line += " \(String(cString: f))"; weaveffi_free_string(f)
            }
            if let l = weaveffi_contacts_Contact_get_last_name(contact) {
                line += " \(String(cString: l))"; weaveffi_free_string(l)
            }
            if let e = weaveffi_contacts_Contact_get_email(contact) {
                line += " <\(String(cString: e))>"; weaveffi_free_string(e)
            }

            let ct = weaveffi_contacts_Contact_get_contact_type(contact)
            let label: String
            switch ct {
            case 0: label = "Personal"
            case 1: label = "Work"
            case 2: label = "Other"
            default: label = "Unknown"
            }
            line += " (\(label))"
            print(line)
        }
        weaveffi_contacts_Contact_list_free(list, len)
    }
}

run()
