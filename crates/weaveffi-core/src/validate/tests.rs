//! The validator's test suite. Fixtures are built in memory (no on-disk
//! IDL), so spans/snippets are exercised separately via the CLI tests.

use super::*;
use weaveffi_ir::ir::{
    Api, CallbackDef, EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function, ListenerDef, Module,
    Param, StructDef, StructField, TypeRef,
};

fn simple_function(name: &str) -> Function {
    Function {
        name: name.to_string(),
        params: vec![Param {
            name: "x".to_string(),
            ty: TypeRef::I32,
            mutable: false,
            doc: None,
        }],
        returns: Some(TypeRef::I32),
        doc: None,
        throws: false,
        r#async: false,
        cancellable: false,
        deprecated: None,
        since: None,
    }
}

fn simple_module(name: &str) -> Module {
    Module {
        name: name.to_string(),
        functions: vec![simple_function("do_stuff")],
        interfaces: vec![],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        modules: vec![],
    }
}

fn simple_api() -> Api {
    Api {
        version: "0.5.0".to_string(),
        modules: vec![simple_module("mymod")],
        generators: None,
        package: None,
    }
}

#[test]
fn valid_api_passes() {
    let mut api = simple_api();
    assert!(validate_api(&mut api, None).is_ok());
}

#[test]
fn duplicate_module_names_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![simple_module("dup"), simple_module("dup")],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::DuplicateModuleName(n) if n == "dup"
    ));
}

#[test]
fn duplicate_function_names_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![simple_function("same"), simple_function("same")],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::DuplicateFunctionName { .. }
    ));
}

#[test]
fn reserved_keywords_rejected() {
    for kw in ["type", "async"] {
        let mut api = Api {
            version: "0.5.0".to_string(),
            modules: vec![Module {
                name: kw.to_string(),
                functions: vec![simple_function("ok_fn")],
                interfaces: vec![],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
            package: None,
        };
        assert!(
            validate_api(&mut api, None).is_err(),
            "Expected reserved keyword '{kw}' to be rejected"
        );
    }
}

#[test]
fn invalid_identifiers_rejected() {
    for bad in ["123", "has spaces", ""] {
        let mut api = Api {
            version: "0.5.0".to_string(),
            modules: vec![Module {
                name: bad.to_string(),
                functions: vec![simple_function("ok_fn")],
                interfaces: vec![],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
            package: None,
        };
        assert!(
            validate_api(&mut api, None).is_err(),
            "Expected invalid identifier '{bad}' to be rejected"
        );
    }
}

#[test]
fn async_function_passes_validation() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![Function {
                name: "do_async".to_string(),
                params: vec![],
                returns: None,
                doc: None,
                throws: false,
                r#async: true,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(validate_api(&mut api, None).is_ok());
}

#[test]
fn async_function_with_return_passes() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![Function {
                name: "fetch_data".to_string(),
                params: vec![Param {
                    name: "url".to_string(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::StringUtf8),
                doc: None,
                throws: false,
                r#async: true,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(validate_api(&mut api, None).is_ok());
}

#[test]
fn async_void_function_emits_warning() {
    let api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![Function {
                name: "fire_and_forget".to_string(),
                params: vec![],
                returns: None,
                doc: Some("documented".to_string()),
                throws: false,
                r#async: true,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    let warnings = collect_warnings(&api);
    assert!(warnings.iter().any(|w| matches!(
        w,
        ValidationWarning::AsyncVoidFunction { module, function }
            if module == "mymod" && function == "fire_and_forget"
    )));
}

#[test]
fn async_function_with_return_no_void_warning() {
    let api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![Function {
                name: "fetch".to_string(),
                params: vec![],
                returns: Some(TypeRef::StringUtf8),
                doc: Some("documented".to_string()),
                throws: false,
                r#async: true,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    let warnings = collect_warnings(&api);
    assert!(!warnings
        .iter()
        .any(|w| matches!(w, ValidationWarning::AsyncVoidFunction { .. })));
}

#[test]
fn empty_module_name_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "".to_string(),
            functions: vec![simple_function("ok_fn")],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::NoModuleName
    ));
}

#[test]
fn doc_example_error_domain_validates() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "contacts".to_string(),
            functions: vec![
                Function {
                    name: "create_contact".to_string(),
                    params: vec![
                        Param {
                            name: "name".to_string(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "email".to_string(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                            doc: None,
                        },
                    ],
                    returns: Some(TypeRef::Handle),
                    doc: None,
                    throws: false,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "get_contact".to_string(),
                    params: vec![Param {
                        name: "id".to_string(),
                        ty: TypeRef::Handle,
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::StringUtf8),
                    doc: None,
                    throws: false,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: Some(ErrorDomain {
                name: "ContactErrors".to_string(),
                codes: vec![
                    ErrorCode {
                        name: "not_found".to_string(),
                        code: 1,
                        message: "Contact not found".to_string(),
                        doc: None,
                    },
                    ErrorCode {
                        name: "duplicate".to_string(),
                        code: 2,
                        message: "Contact already exists".to_string(),
                        doc: None,
                    },
                    ErrorCode {
                        name: "invalid_email".to_string(),
                        code: 3,
                        message: "Email address is invalid".to_string(),
                        doc: None,
                    },
                ],
            }),
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(validate_api(&mut api, None).is_ok());
}

#[test]
fn error_code_zero_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![simple_function("ok_fn")],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: Some(ErrorDomain {
                name: "MyErrors".to_string(),
                codes: vec![ErrorCode {
                    name: "success".to_string(),
                    code: 0,
                    message: "should fail".to_string(),
                    doc: None,
                }],
            }),
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::InvalidErrorCode { module, name }
            if module == "mymod" && name == "success"
    ));
}

#[test]
fn error_domain_name_collision_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![simple_function("do_stuff")],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: Some(ErrorDomain {
                name: "do_stuff".to_string(),
                codes: vec![ErrorCode {
                    name: "fail".to_string(),
                    code: 1,
                    message: "failed".to_string(),
                    doc: None,
                }],
            }),
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::NameCollisionWithErrorDomain { module, name }
            if module == "mymod" && name == "do_stuff"
    ));
}

#[test]
fn duplicate_error_names_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![simple_function("ok_fn")],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: Some(ErrorDomain {
                name: "MyErrors".to_string(),
                codes: vec![
                    ErrorCode {
                        name: "fail".to_string(),
                        code: 1,
                        message: "failed".to_string(),
                        doc: None,
                    },
                    ErrorCode {
                        name: "fail".to_string(),
                        code: 2,
                        message: "also failed".to_string(),
                        doc: None,
                    },
                ],
            }),
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::DuplicateErrorName { module, name }
            if module == "mymod" && name == "fail"
    ));
}

#[test]
fn duplicate_error_codes_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![simple_function("ok_fn")],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: Some(ErrorDomain {
                name: "MyErrors".to_string(),
                codes: vec![
                    ErrorCode {
                        name: "not_found".to_string(),
                        code: 1,
                        message: "not found".to_string(),
                        doc: None,
                    },
                    ErrorCode {
                        name: "timeout".to_string(),
                        code: 1,
                        message: "timed out".to_string(),
                        doc: None,
                    },
                ],
            }),
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::DuplicateErrorCode { .. }
    ));
}

#[test]
fn duplicate_error_code_names_across_domains_rejected() {
    let domain = |type_name: &str, code_name: &str| {
        Some(ErrorDomain {
            name: type_name.to_string(),
            codes: vec![ErrorCode {
                name: code_name.to_string(),
                code: 1,
                message: "gone".to_string(),
                doc: None,
            }],
        })
    };
    let module = |name: &str, errors| Module {
        name: name.to_string(),
        functions: vec![simple_function("ok_fn")],
        interfaces: vec![],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors,
        modules: vec![],
    };
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![
            module("products", domain("ProductsError", "NotFound")),
            module("orders", domain("OrdersError", "NotFound")),
        ],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::DuplicateErrorCodeName { name, first, second }
            if name == "NotFound"
                && first == "products.ProductsError"
                && second == "orders.OrdersError"
    ));

    // Distinct code names across domains stay valid.
    let mut ok = Api {
        version: "0.5.0".to_string(),
        modules: vec![
            module("products", domain("ProductsError", "ProductNotFound")),
            module("orders", domain("OrdersError", "OrderNotFound")),
        ],
        generators: None,
        package: None,
    };
    assert!(validate_api(&mut ok, None).is_ok());
}

fn simple_struct(name: &str) -> StructDef {
    StructDef {
        name: name.to_string(),
        doc: None,
        fields: vec![StructField {
            name: "x".to_string(),
            ty: TypeRef::I32,
            doc: None,
            default: None,
        }],
        builder: false,
    }
}

#[test]
fn duplicate_struct_names_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![simple_function("ok_fn")],
            interfaces: vec![],
            structs: vec![simple_struct("Point"), simple_struct("Point")],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::DuplicateStructName { module, name }
            if module == "mymod" && name == "Point"
    ));
}

#[test]
fn empty_struct_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![simple_function("ok_fn")],
            interfaces: vec![],
            structs: vec![StructDef {
                name: "Empty".to_string(),
                doc: None,
                fields: vec![],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::EmptyStruct { module, name }
            if module == "mymod" && name == "Empty"
    ));
}

#[test]
fn duplicate_struct_field_names_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![simple_function("ok_fn")],
            interfaces: vec![],
            structs: vec![StructDef {
                name: "Point".to_string(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "x".to_string(),
                        ty: TypeRef::I32,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "x".to_string(),
                        ty: TypeRef::F64,
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::DuplicateStructField { struct_name, field }
            if struct_name == "Point" && field == "x"
    ));
}

fn simple_enum(name: &str) -> EnumDef {
    EnumDef {
        name: name.to_string(),
        doc: None,
        variants: vec![
            EnumVariant {
                name: "A".to_string(),
                value: 0,
                doc: None,
                fields: vec![],
            },
            EnumVariant {
                name: "B".to_string(),
                value: 1,
                doc: None,
                fields: vec![],
            },
        ],
    }
}

#[test]
fn duplicate_enum_names_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![simple_function("ok_fn")],
            interfaces: vec![],
            structs: vec![],
            enums: vec![simple_enum("Color"), simple_enum("Color")],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::DuplicateEnumName { module, name }
            if module == "mymod" && name == "Color"
    ));
}

#[test]
fn empty_enum_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![simple_function("ok_fn")],
            interfaces: vec![],
            structs: vec![],
            enums: vec![EnumDef {
                name: "Empty".to_string(),
                doc: None,
                variants: vec![],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::EmptyEnum { module, name }
            if module == "mymod" && name == "Empty"
    ));
}

#[test]
fn duplicate_enum_variant_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![simple_function("ok_fn")],
            interfaces: vec![],
            structs: vec![],
            enums: vec![EnumDef {
                name: "Color".to_string(),
                doc: None,
                variants: vec![
                    EnumVariant {
                        name: "Red".to_string(),
                        value: 0,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Red".to_string(),
                        value: 1,
                        doc: None,
                        fields: vec![],
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::DuplicateEnumVariant { enum_name, variant }
            if enum_name == "Color" && variant == "Red"
    ));
}

#[test]
fn duplicate_enum_value_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![simple_function("ok_fn")],
            interfaces: vec![],
            structs: vec![],
            enums: vec![EnumDef {
                name: "Color".to_string(),
                doc: None,
                variants: vec![
                    EnumVariant {
                        name: "Red".to_string(),
                        value: 0,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Green".to_string(),
                        value: 0,
                        doc: None,
                        fields: vec![],
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::DuplicateEnumValue { enum_name, value }
            if enum_name == "Color" && *value == 0
    ));
}

#[test]
fn unknown_type_ref_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![Function {
                name: "do_stuff".to_string(),
                params: vec![Param {
                    name: "x".to_string(),
                    ty: TypeRef::Struct("Foo".to_string()),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::UnknownTypeRef { name } if name == "Foo"
    ));
}

#[test]
fn valid_struct_ref_passes() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![Function {
                name: "do_stuff".to_string(),
                params: vec![Param {
                    name: "p".to_string(),
                    ty: TypeRef::Struct("Point".to_string()),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![simple_struct("Point")],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(validate_api(&mut api, None).is_ok());
}

#[test]
fn unknown_type_ref_in_optional_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![Function {
                name: "do_stuff".to_string(),
                params: vec![Param {
                    name: "x".to_string(),
                    ty: TypeRef::Optional(Box::new(TypeRef::Struct("Bar".to_string()))),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::UnknownTypeRef { name } if name == "Bar"
    ));
}

#[test]
fn unknown_type_ref_in_list_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![Function {
                name: "do_stuff".to_string(),
                params: vec![],
                returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Baz".to_string())))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::UnknownTypeRef { name } if name == "Baz"
    ));
}

#[test]
fn struct_field_referencing_unknown_type() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![simple_function("ok_fn")],
            interfaces: vec![],
            structs: vec![StructDef {
                name: "Wrapper".to_string(),
                doc: None,
                fields: vec![StructField {
                    name: "inner".to_string(),
                    ty: TypeRef::Struct("Nonexistent".to_string()),
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::UnknownTypeRef { name } if name == "Nonexistent"
    ));
}

#[test]
fn function_param_with_optional_struct() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![Function {
                name: "save".to_string(),
                params: vec![Param {
                    name: "c".to_string(),
                    ty: TypeRef::Optional(Box::new(TypeRef::Struct("Contact".to_string()))),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![StructDef {
                name: "Contact".to_string(),
                doc: None,
                fields: vec![StructField {
                    name: "name".to_string(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(validate_api(&mut api, None).is_ok());
}

#[test]
fn function_param_with_list_of_enums() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![Function {
                name: "paint".to_string(),
                params: vec![Param {
                    name: "colors".to_string(),
                    ty: TypeRef::List(Box::new(TypeRef::Enum("Color".to_string()))),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![simple_enum("Color")],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(validate_api(&mut api, None).is_ok());
}

#[test]
fn nested_optional_list_validates() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![Function {
                name: "list_contacts".to_string(),
                params: vec![],
                returns: Some(TypeRef::List(Box::new(TypeRef::Optional(Box::new(
                    TypeRef::Struct("Contact".to_string()),
                ))))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![StructDef {
                name: "Contact".to_string(),
                doc: None,
                fields: vec![StructField {
                    name: "name".to_string(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(validate_api(&mut api, None).is_ok());
}

#[test]
fn list_of_list_param_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![Function {
                name: "f".to_string(),
                params: vec![Param {
                    name: "data".to_string(),
                    ty: TypeRef::List(Box::new(TypeRef::List(Box::new(TypeRef::I32)))),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    let err = validate_api(&mut api, None).unwrap_err();
    assert!(
        matches!(
            &err.first().error,
            ValidationError::UnsupportedElementType { location, .. }
                if location == "param 'data' of function 'mymod::f'"
        ),
        "expected UnsupportedElementType, got: {:?}",
        err.first().error
    );
}

#[test]
fn list_of_optional_scalar_return_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![Function {
                name: "f".to_string(),
                params: vec![],
                returns: Some(TypeRef::List(Box::new(TypeRef::Optional(Box::new(
                    TypeRef::I32,
                ))))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    let err = validate_api(&mut api, None).unwrap_err();
    assert!(
        matches!(
            &err.first().error,
            ValidationError::UnsupportedElementType { .. }
        ),
        "scalar arrays cannot express per-element null; got: {:?}",
        err.first().error
    );
}

#[test]
fn map_of_struct_value_field_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![simple_function("ok_fn")],
            interfaces: vec![],
            structs: vec![StructDef {
                name: "Widget".to_string(),
                doc: None,
                fields: vec![StructField {
                    name: "parts".to_string(),
                    ty: TypeRef::Map(
                        Box::new(TypeRef::StringUtf8),
                        Box::new(TypeRef::Struct("Widget".to_string())),
                    ),
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    let err = validate_api(&mut api, None).unwrap_err();
    assert!(
        matches!(
            &err.first().error,
            ValidationError::UnsupportedElementType { location, .. }
                if location == "field 'parts' of struct 'Widget'"
        ),
        "expected UnsupportedElementType, got: {:?}",
        err.first().error
    );
}

#[test]
fn enum_variant_value_zero_allowed() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![simple_function("ok_fn")],
            interfaces: vec![],
            structs: vec![],
            enums: vec![EnumDef {
                name: "Status".to_string(),
                doc: None,
                variants: vec![
                    EnumVariant {
                        name: "Unknown".to_string(),
                        value: 0,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Active".to_string(),
                        value: 1,
                        doc: None,
                        fields: vec![],
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(validate_api(&mut api, None).is_ok());
}

#[test]
fn valid_enum_ref_passes() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![Function {
                name: "get_color".to_string(),
                params: vec![],
                returns: Some(TypeRef::Enum("Color".to_string())),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![simple_enum("Color")],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(validate_api(&mut api, None).is_ok());
}

#[test]
fn resolve_enum_ref_in_function_param() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![Function {
                name: "paint".to_string(),
                params: vec![Param {
                    name: "color".to_string(),
                    ty: TypeRef::Struct("Color".to_string()),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![simple_enum("Color")],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    validate_api(&mut api, None).unwrap();
    assert_eq!(
        api.modules[0].functions[0].params[0].ty,
        TypeRef::Enum("Color".to_string())
    );
}

#[test]
fn resolve_enum_ref_in_optional() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![Function {
                name: "paint".to_string(),
                params: vec![Param {
                    name: "color".to_string(),
                    ty: TypeRef::Optional(Box::new(TypeRef::Struct("Color".to_string()))),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![simple_enum("Color")],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    validate_api(&mut api, None).unwrap();
    assert_eq!(
        api.modules[0].functions[0].params[0].ty,
        TypeRef::Optional(Box::new(TypeRef::Enum("Color".to_string())))
    );
}

#[test]
fn struct_ref_not_changed() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![Function {
                name: "save".to_string(),
                params: vec![Param {
                    name: "c".to_string(),
                    ty: TypeRef::Struct("Contact".to_string()),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![simple_struct("Contact")],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    validate_api(&mut api, None).unwrap();
    assert_eq!(
        api.modules[0].functions[0].params[0].ty,
        TypeRef::Struct("Contact".to_string())
    );
}

#[test]
fn map_with_string_key_passes() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![Function {
                name: "get_map".to_string(),
                params: vec![],
                returns: Some(TypeRef::Map(
                    Box::new(TypeRef::StringUtf8),
                    Box::new(TypeRef::I32),
                )),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(validate_api(&mut api, None).is_ok());
}

#[test]
fn map_with_struct_key_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![Function {
                name: "get_map".to_string(),
                params: vec![],
                returns: Some(TypeRef::Map(
                    Box::new(TypeRef::Struct("Point".to_string())),
                    Box::new(TypeRef::I32),
                )),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![simple_struct("Point")],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::InvalidMapKey { key_type } if key_type == "struct Point"
    ));
}

#[test]
fn map_with_enum_key_passes() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![Function {
                name: "get_map".to_string(),
                params: vec![],
                returns: Some(TypeRef::Map(
                    Box::new(TypeRef::Enum("Color".to_string())),
                    Box::new(TypeRef::StringUtf8),
                )),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![simple_enum("Color")],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(validate_api(&mut api, None).is_ok());
}

#[test]
fn warning_large_enum_variant_count() {
    let variants: Vec<EnumVariant> = (0..101)
        .map(|i| EnumVariant {
            name: format!("V{i}"),
            value: i,
            doc: None,
            fields: vec![],
        })
        .collect();
    let api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![simple_function("ok_fn")],
            interfaces: vec![],
            structs: vec![],
            enums: vec![EnumDef {
                name: "BigEnum".to_string(),
                doc: None,
                variants,
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    let warnings = collect_warnings(&api);
    assert!(warnings.iter().any(|w| matches!(
        w,
        ValidationWarning::LargeEnumVariantCount { enum_name, count }
            if enum_name == "BigEnum" && *count == 101
    )));
}

#[test]
fn warning_enum_at_100_no_warning() {
    let variants: Vec<EnumVariant> = (0..100)
        .map(|i| EnumVariant {
            name: format!("V{i}"),
            value: i,
            doc: None,
            fields: vec![],
        })
        .collect();
    let api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![simple_function("ok_fn")],
            interfaces: vec![],
            structs: vec![],
            enums: vec![EnumDef {
                name: "BigEnum".to_string(),
                doc: None,
                variants,
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    let warnings = collect_warnings(&api);
    assert!(!warnings
        .iter()
        .any(|w| matches!(w, ValidationWarning::LargeEnumVariantCount { .. })));
}

#[test]
fn warning_deep_nesting_in_param() {
    let deep = TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::Optional(
        Box::new(TypeRef::List(Box::new(TypeRef::I32))),
    )))));
    let api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![Function {
                name: "nested_fn".to_string(),
                params: vec![Param {
                    name: "data".to_string(),
                    ty: deep,
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: Some("documented".to_string()),
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    let warnings = collect_warnings(&api);
    assert!(warnings.iter().any(|w| matches!(
        w,
        ValidationWarning::DeepNesting { location, depth }
            if location == "mymod::nested_fn::data" && *depth == 4
    )));
}

#[test]
fn warning_nesting_at_3_no_warning() {
    let nested = TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::Optional(
        Box::new(TypeRef::I32),
    )))));
    let api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![Function {
                name: "ok_fn".to_string(),
                params: vec![Param {
                    name: "data".to_string(),
                    ty: nested,
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: Some("documented".to_string()),
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    let warnings = collect_warnings(&api);
    assert!(!warnings
        .iter()
        .any(|w| matches!(w, ValidationWarning::DeepNesting { .. })));
}

#[test]
fn warning_deep_nesting_in_struct_field() {
    let deep = TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::Optional(
        Box::new(TypeRef::List(Box::new(TypeRef::I32))),
    )))));
    let api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![simple_function("ok_fn")],
            interfaces: vec![],
            structs: vec![StructDef {
                name: "Widget".to_string(),
                doc: None,
                fields: vec![StructField {
                    name: "data".to_string(),
                    ty: deep,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    let warnings = collect_warnings(&api);
    assert!(warnings.iter().any(|w| matches!(
        w,
        ValidationWarning::DeepNesting { location, .. }
            if location == "mymod::Widget::data"
    )));
}

#[test]
fn warning_empty_module_doc() {
    let api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "undocumented".to_string(),
            functions: vec![
                Function {
                    name: "a".to_string(),
                    params: vec![],
                    returns: None,
                    doc: None,
                    throws: false,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "b".to_string(),
                    params: vec![],
                    returns: None,
                    doc: None,
                    throws: false,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    let warnings = collect_warnings(&api);
    assert!(warnings.iter().any(|w| matches!(
        w,
        ValidationWarning::EmptyModuleDoc { module } if module == "undocumented"
    )));
}

#[test]
fn warning_partial_docs_no_warning() {
    let api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "partial".to_string(),
            functions: vec![
                Function {
                    name: "a".to_string(),
                    params: vec![],
                    returns: None,
                    doc: Some("has doc".to_string()),
                    throws: false,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "b".to_string(),
                    params: vec![],
                    returns: None,
                    doc: None,
                    throws: false,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    let warnings = collect_warnings(&api);
    assert!(!warnings
        .iter()
        .any(|w| matches!(w, ValidationWarning::EmptyModuleDoc { .. })));
}

#[test]
fn warning_no_functions_no_empty_doc_warning() {
    let api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "empty".to_string(),
            functions: vec![],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    let warnings = collect_warnings(&api);
    assert!(!warnings
        .iter()
        .any(|w| matches!(w, ValidationWarning::EmptyModuleDoc { .. })));
}

#[test]
fn warning_clean_api_no_warnings() {
    let api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "clean".to_string(),
            functions: vec![Function {
                name: "add".to_string(),
                params: vec![Param {
                    name: "x".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::I32),
                doc: Some("Adds numbers".to_string()),
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![simple_enum("Color")],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    let warnings = collect_warnings(&api);
    assert!(warnings.is_empty());
}

#[test]
fn resolve_enum_ref_in_struct_field() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![simple_function("ok_fn")],
            interfaces: vec![],
            structs: vec![StructDef {
                name: "Widget".to_string(),
                doc: None,
                fields: vec![StructField {
                    name: "color".to_string(),
                    ty: TypeRef::Struct("Color".to_string()),
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![simple_enum("Color")],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    validate_api(&mut api, None).unwrap();
    assert_eq!(
        api.modules[0].structs[0].fields[0].ty,
        TypeRef::Enum("Color".to_string())
    );
}

#[test]
fn typed_handle_valid_struct_passes() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![Function {
                name: "get_session".to_string(),
                params: vec![Param {
                    name: "h".to_string(),
                    ty: TypeRef::TypedHandle("Session".to_string()),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![simple_struct("Session")],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(validate_api(&mut api, None).is_ok());
}

#[test]
fn typed_handle_unknown_struct_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "mymod".to_string(),
            functions: vec![Function {
                name: "get_session".to_string(),
                params: vec![Param {
                    name: "h".to_string(),
                    ty: TypeRef::TypedHandle("Nonexistent".to_string()),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::UnknownTypeRef { name } if name == "Nonexistent"
    ));
}

#[test]
fn borrowed_str_param_accepted() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "io".to_string(),
            functions: vec![Function {
                name: "write".to_string(),
                params: vec![Param {
                    name: "data".to_string(),
                    ty: TypeRef::BorrowedStr,
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(validate_api(&mut api, None).is_ok());
}

#[test]
fn borrowed_bytes_param_accepted() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "io".to_string(),
            functions: vec![Function {
                name: "upload".to_string(),
                params: vec![Param {
                    name: "raw".to_string(),
                    ty: TypeRef::BorrowedBytes,
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(validate_api(&mut api, None).is_ok());
}

#[test]
fn borrowed_str_in_return_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "io".to_string(),
            functions: vec![Function {
                name: "read".to_string(),
                params: vec![],
                returns: Some(TypeRef::BorrowedStr),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::BorrowedTypeInInvalidPosition { ty, location }
            if ty == "&str" && location.contains("return type")
    ));
}

#[test]
fn borrowed_bytes_in_return_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "io".to_string(),
            functions: vec![Function {
                name: "read_raw".to_string(),
                params: vec![],
                returns: Some(TypeRef::BorrowedBytes),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::BorrowedTypeInInvalidPosition { ty, location }
            if ty == "&[u8]" && location.contains("return type")
    ));
}

#[test]
fn borrowed_str_in_struct_field_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "data".to_string(),
            functions: vec![],
            interfaces: vec![],
            structs: vec![StructDef {
                name: "Msg".to_string(),
                fields: vec![StructField {
                    name: "text".to_string(),
                    ty: TypeRef::BorrowedStr,
                    doc: None,
                    default: None,
                }],
                builder: false,
                doc: None,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::BorrowedTypeInInvalidPosition { ty, location }
            if ty == "&str" && location.contains("struct")
    ));
}

#[test]
fn borrowed_bytes_in_struct_field_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "data".to_string(),
            functions: vec![],
            interfaces: vec![],
            structs: vec![StructDef {
                name: "Blob".to_string(),
                fields: vec![StructField {
                    name: "content".to_string(),
                    ty: TypeRef::BorrowedBytes,
                    doc: None,
                    default: None,
                }],
                builder: false,
                doc: None,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::BorrowedTypeInInvalidPosition { ty, location }
            if ty == "&[u8]" && location.contains("struct")
    ));
}

#[test]
fn borrowed_str_nested_in_optional_return_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "io".to_string(),
            functions: vec![Function {
                name: "maybe_read".to_string(),
                params: vec![],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::BorrowedStr))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::BorrowedTypeInInvalidPosition { ty, .. }
            if ty == "&str"
    ));
}

#[test]
fn cross_module_struct_ref_passes() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![
            Module {
                name: "orders".to_string(),
                functions: vec![Function {
                    name: "place_order".to_string(),
                    params: vec![Param {
                        name: "item".to_string(),
                        ty: TypeRef::Struct("Product".to_string()),
                        mutable: false,
                        doc: None,
                    }],
                    returns: None,
                    doc: None,
                    throws: false,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                interfaces: vec![],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            },
            Module {
                name: "catalog".to_string(),
                functions: vec![simple_function("list_products")],
                interfaces: vec![],
                structs: vec![simple_struct("Product")],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            },
        ],
        generators: None,
        package: None,
    };
    validate_api(&mut api, None).unwrap();
    assert_eq!(
        api.modules[0].functions[0].params[0].ty,
        TypeRef::Struct("catalog.Product".to_string())
    );
}

#[test]
fn cross_module_enum_ref_passes() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![
            Module {
                name: "orders".to_string(),
                functions: vec![Function {
                    name: "get_status".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::Struct("Status".to_string())),
                    doc: None,
                    throws: false,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                interfaces: vec![],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            },
            Module {
                name: "shared".to_string(),
                functions: vec![simple_function("noop")],
                interfaces: vec![],
                structs: vec![],
                enums: vec![simple_enum("Status")],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            },
        ],
        generators: None,
        package: None,
    };
    validate_api(&mut api, None).unwrap();
    assert_eq!(
        api.modules[0].functions[0].returns,
        Some(TypeRef::Enum("shared.Status".to_string()))
    );
}

#[test]
fn cross_module_unknown_still_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![
            Module {
                name: "orders".to_string(),
                functions: vec![Function {
                    name: "do_stuff".to_string(),
                    params: vec![Param {
                        name: "x".to_string(),
                        ty: TypeRef::Struct("Nonexistent".to_string()),
                        mutable: false,
                        doc: None,
                    }],
                    returns: None,
                    doc: None,
                    throws: false,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                interfaces: vec![],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            },
            Module {
                name: "catalog".to_string(),
                functions: vec![simple_function("list_products")],
                interfaces: vec![],
                structs: vec![simple_struct("Product")],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            },
        ],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::UnknownTypeRef { name } if name == "Nonexistent"
    ));
}

#[test]
fn find_type_in_api_finds_struct() {
    let api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "catalog".to_string(),
            functions: vec![],
            interfaces: vec![],
            structs: vec![simple_struct("Product")],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    let result = find_type_in_api(&api, "Product");
    assert_eq!(result, Some(("catalog".to_string(), false)));
}

#[test]
fn find_type_in_api_finds_enum() {
    let api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "shared".to_string(),
            functions: vec![],
            interfaces: vec![],
            structs: vec![],
            enums: vec![simple_enum("Status")],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    let result = find_type_in_api(&api, "Status");
    assert_eq!(result, Some(("shared".to_string(), true)));
}

#[test]
fn find_type_in_api_returns_none_for_unknown() {
    let api = Api {
        version: "0.5.0".to_string(),
        modules: vec![simple_module("mymod")],
        generators: None,
        package: None,
    };
    assert_eq!(find_type_in_api(&api, "Nonexistent"), None);
}

#[test]
fn validate_nested_module_passes() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "parent".to_string(),
            functions: vec![simple_function("top_fn")],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![Module {
                name: "child".to_string(),
                functions: vec![simple_function("inner_fn")],
                interfaces: vec![],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
        }],
        generators: None,
        package: None,
    };
    assert!(validate_api(&mut api, None).is_ok());
}

fn function_returning(name: &str, ret: TypeRef) -> Function {
    Function {
        name: name.to_string(),
        params: vec![],
        returns: Some(ret),
        doc: None,
        throws: false,
        r#async: false,
        cancellable: false,
        deprecated: None,
        since: None,
    }
}

#[test]
fn find_type_in_api_finds_nested_type() {
    let api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "graphics".to_string(),
            functions: vec![],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![Module {
                name: "shapes".to_string(),
                functions: vec![],
                interfaces: vec![],
                structs: vec![simple_struct("Circle")],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
        }],
        generators: None,
        package: None,
    };
    // Nested types report their full dot-joined owner path.
    assert_eq!(
        find_type_in_api(&api, "Circle"),
        Some(("graphics.shapes".to_string(), false))
    );
}

#[test]
fn resolve_qualifies_reference_to_nested_module_type() {
    // A top-level module references a struct defined in one of its own
    // nested submodules by bare name; resolution must qualify it to the
    // full dotted path so codegen can mangle the right C symbol.
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "app".to_string(),
            functions: vec![function_returning(
                "make",
                TypeRef::Struct("Widget".to_string()),
            )],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![Module {
                name: "ui".to_string(),
                functions: vec![],
                interfaces: vec![],
                structs: vec![simple_struct("Widget")],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
        }],
        generators: None,
        package: None,
    };
    validate_api(&mut api, None).unwrap();
    assert_eq!(
        api.modules[0].functions[0].returns,
        Some(TypeRef::Struct("app.ui.Widget".to_string()))
    );
}

#[test]
fn resolve_qualifies_nested_module_reference_to_parent_type() {
    // A nested module references a type owned by an ancestor module. Before
    // the nested-aware resolver, refs inside submodules were never
    // qualified at all; now they resolve to the owner's dotted path.
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "lib".to_string(),
            functions: vec![],
            interfaces: vec![],
            structs: vec![simple_struct("Token")],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![Module {
                name: "inner".to_string(),
                functions: vec![function_returning(
                    "fetch",
                    TypeRef::Struct("Token".to_string()),
                )],
                interfaces: vec![],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
        }],
        generators: None,
        package: None,
    };
    validate_api(&mut api, None).unwrap();
    assert_eq!(
        api.modules[0].modules[0].functions[0].returns,
        Some(TypeRef::Struct("lib.Token".to_string()))
    );
}

#[test]
fn resolve_qualifies_nested_module_typed_handle_to_parent_type() {
    // Regression: a `handle<T>` inside a submodule whose target struct is
    // owned by an ancestor module must be qualified to the owner's path.
    // Previously the resolver had no `TypedHandle` arm, so it stayed bare
    // and every consumer (C ABI lowering + language wrappers) mis-attributed
    // it to the *referrer's* prefix (e.g. `weaveffi_kv_stats_Store` instead
    // of `weaveffi_kv_Store`), producing an undeclared type.
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "kv".to_string(),
            functions: vec![],
            interfaces: vec![],
            structs: vec![simple_struct("Store")],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![Module {
                name: "stats".to_string(),
                functions: vec![function_returning(
                    "get_store",
                    TypeRef::TypedHandle("Store".to_string()),
                )],
                interfaces: vec![],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
        }],
        generators: None,
        package: None,
    };
    validate_api(&mut api, None).unwrap();
    assert_eq!(
        api.modules[0].modules[0].functions[0].returns,
        Some(TypeRef::TypedHandle("kv.Store".to_string()))
    );
}

#[test]
fn resolve_keeps_same_module_typed_handle_unqualified() {
    // A `handle<T>` whose target is defined in the *same* module stays bare
    // so the lowering keeps using the current module's prefix.
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "sessions".to_string(),
            functions: vec![function_returning(
                "open",
                TypeRef::TypedHandle("Session".to_string()),
            )],
            interfaces: vec![],
            structs: vec![simple_struct("Session")],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    validate_api(&mut api, None).unwrap();
    assert_eq!(
        api.modules[0].functions[0].returns,
        Some(TypeRef::TypedHandle("Session".to_string()))
    );
}

#[test]
fn resolve_converts_nested_enum_reference_to_enum_variant() {
    // An unqualified reference to an enum that lives in another module must
    // be rewritten as `TypeRef::Enum` (not `Struct`) with the dotted path,
    // using the global index's `is_enum` flag.
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![
            Module {
                name: "consumer".to_string(),
                functions: vec![function_returning(
                    "status",
                    TypeRef::Struct("Phase".to_string()),
                )],
                interfaces: vec![],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            },
            Module {
                name: "shared".to_string(),
                functions: vec![],
                interfaces: vec![],
                structs: vec![],
                enums: vec![simple_enum("Phase")],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            },
        ],
        generators: None,
        package: None,
    };
    validate_api(&mut api, None).unwrap();
    assert_eq!(
        api.modules[0].functions[0].returns,
        Some(TypeRef::Enum("shared.Phase".to_string()))
    );
}

#[test]
fn duplicate_callback_names_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "events".to_string(),
            functions: vec![],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![
                CallbackDef {
                    name: "on_data".to_string(),
                    params: vec![],
                    doc: None,
                },
                CallbackDef {
                    name: "on_data".to_string(),
                    params: vec![],
                    doc: None,
                },
            ],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::DuplicateCallbackName { module, name }
            if module == "events" && name == "on_data"
    ));
}

#[test]
fn listener_referencing_undefined_callback_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "events".to_string(),
            functions: vec![],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![ListenerDef {
                name: "watcher".to_string(),
                event_callback: "nonexistent".to_string(),
                doc: None,
            }],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::ListenerCallbackNotFound { module, listener, callback }
            if module == "events" && listener == "watcher" && callback == "nonexistent"
    ));
}

#[test]
fn listener_referencing_defined_callback_passes() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "events".to_string(),
            functions: vec![],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![CallbackDef {
                name: "on_data".to_string(),
                params: vec![Param {
                    name: "payload".to_string(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
                }],
                doc: None,
            }],
            listeners: vec![ListenerDef {
                name: "data_stream".to_string(),
                event_callback: "on_data".to_string(),
                doc: Some("Subscribe to data".to_string()),
            }],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(validate_api(&mut api, None).is_ok());
}

#[test]
fn duplicate_listener_names_rejected() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "events".to_string(),
            functions: vec![],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![CallbackDef {
                name: "on_data".to_string(),
                params: vec![],
                doc: None,
            }],
            listeners: vec![
                ListenerDef {
                    name: "watcher".to_string(),
                    event_callback: "on_data".to_string(),
                    doc: None,
                },
                ListenerDef {
                    name: "watcher".to_string(),
                    event_callback: "on_data".to_string(),
                    doc: None,
                },
            ],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::DuplicateListenerName { module, name }
            if module == "events" && name == "watcher"
    ));
}

#[test]
fn iterator_valid_as_return_type() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "data".to_string(),
            functions: vec![Function {
                name: "list_items".to_string(),
                params: vec![],
                returns: Some(TypeRef::Iterator(Box::new(TypeRef::I32))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(validate_api(&mut api, None).is_ok());
}

#[test]
fn iterator_rejected_as_param() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "data".to_string(),
            functions: vec![Function {
                name: "consume".to_string(),
                params: vec![Param {
                    name: "items".to_string(),
                    ty: TypeRef::Iterator(Box::new(TypeRef::I32)),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::IteratorInInvalidPosition { .. }
    ));
}

#[test]
fn iterator_rejected_in_struct_field() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "data".to_string(),
            functions: vec![],
            interfaces: vec![],
            structs: vec![StructDef {
                name: "Container".to_string(),
                doc: None,
                fields: vec![StructField {
                    name: "items".to_string(),
                    ty: TypeRef::Iterator(Box::new(TypeRef::I32)),
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    assert!(matches!(
        &validate_api(&mut api, None).unwrap_err().first().error,
        ValidationError::IteratorInInvalidPosition { .. }
    ));
}

#[test]
fn builder_struct_empty_is_error() {
    let mut api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "m".into(),
            functions: vec![],
            interfaces: vec![],
            structs: vec![StructDef {
                name: "Empty".into(),
                doc: None,
                fields: vec![],
                builder: true,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    let err = validate_api(&mut api, None).unwrap_err();
    assert!(
        matches!(
            err.first().error,
            ValidationError::BuilderStructEmpty { .. }
        ),
        "expected BuilderStructEmpty, got: {err}"
    );
}

#[test]
fn warning_mutable_on_value_type() {
    let api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "math".to_string(),
            functions: vec![Function {
                name: "add".to_string(),
                params: vec![Param {
                    name: "x".to_string(),
                    ty: TypeRef::I32,
                    mutable: true,
                    doc: None,
                }],
                returns: Some(TypeRef::I32),
                doc: Some("add".to_string()),
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    let warnings = collect_warnings(&api);
    assert!(warnings.iter().any(|w| matches!(
        w,
        ValidationWarning::MutableOnValueType {
            param,
            ..
        } if param == "x"
    )));
}

#[test]
fn no_warning_mutable_on_pointer_type() {
    let api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "io".to_string(),
            functions: vec![Function {
                name: "fill".to_string(),
                params: vec![
                    Param {
                        name: "buf".to_string(),
                        ty: TypeRef::Bytes,
                        mutable: true,
                        doc: None,
                    },
                    Param {
                        name: "msg".to_string(),
                        ty: TypeRef::StringUtf8,
                        mutable: true,
                        doc: None,
                    },
                    Param {
                        name: "obj".to_string(),
                        ty: TypeRef::Struct("Thing".into()),
                        mutable: true,
                        doc: None,
                    },
                ],
                returns: None,
                doc: Some("fill".to_string()),
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    let warnings = collect_warnings(&api);
    assert!(
        !warnings
            .iter()
            .any(|w| matches!(w, ValidationWarning::MutableOnValueType { .. })),
        "pointer types should not trigger mutable warning"
    );
}

#[test]
fn no_warning_mutable_false_on_value_type() {
    let api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "math".to_string(),
            functions: vec![Function {
                name: "add".to_string(),
                params: vec![Param {
                    name: "x".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::I32),
                doc: Some("add".to_string()),
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    let warnings = collect_warnings(&api);
    assert!(
        !warnings
            .iter()
            .any(|w| matches!(w, ValidationWarning::MutableOnValueType { .. })),
        "mutable=false should not trigger warning"
    );
}

#[test]
fn warning_mutable_on_enum_type() {
    let api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "paint".to_string(),
            functions: vec![Function {
                name: "set_color".to_string(),
                params: vec![Param {
                    name: "color".to_string(),
                    ty: TypeRef::Enum("Color".into()),
                    mutable: true,
                    doc: None,
                }],
                returns: None,
                doc: Some("set".to_string()),
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    let warnings = collect_warnings(&api);
    assert!(warnings.iter().any(|w| matches!(
        w,
        ValidationWarning::MutableOnValueType { param, .. } if param == "color"
    )));
}

#[test]
fn warning_deprecated_function() {
    let api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "math".to_string(),
            functions: vec![Function {
                name: "add_old".to_string(),
                params: vec![],
                returns: Some(TypeRef::I32),
                doc: Some("old add".to_string()),
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: Some("Use add_v2 instead".to_string()),
                since: Some("0.1.0".to_string()),
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    let warnings = collect_warnings(&api);
    assert!(warnings.iter().any(|w| matches!(
        w,
        ValidationWarning::DeprecatedFunction { function, message, .. }
            if function == "add_old" && message == "Use add_v2 instead"
    )));
}

#[test]
fn no_warning_for_non_deprecated_function() {
    let api = Api {
        version: "0.5.0".to_string(),
        modules: vec![Module {
            name: "math".to_string(),
            functions: vec![Function {
                name: "add".to_string(),
                params: vec![],
                returns: Some(TypeRef::I32),
                doc: Some("add things".to_string()),
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    };
    let warnings = collect_warnings(&api);
    assert!(!warnings
        .iter()
        .any(|w| matches!(w, ValidationWarning::DeprecatedFunction { .. })));
}
