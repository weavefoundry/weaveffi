// End-to-end consumer test for the Dart binding consumers.
//
// Loads the calculator and contacts cdylibs at runtime via dart:ffi
// DynamicLibrary.open and exercises a representative slice of the C
// ABI: add, create_contact, list_contacts, delete_contact. Prints
// "OK" and exits 0 on success; any assertion failure exits 1.

import 'dart:ffi';
import 'dart:io';
import 'package:ffi/ffi.dart';

final class WeaveffiError extends Struct {
  @Int32()
  external int code;
  external Pointer<Utf8> message;
}

typedef AddNative = Int32 Function(Int32, Int32, Pointer<WeaveffiError>);
typedef AddDart = int Function(int, int, Pointer<WeaveffiError>);

typedef CreateNative = Uint64 Function(
    Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>, Int32, Pointer<WeaveffiError>);
typedef CreateDart = int Function(
    Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>, int, Pointer<WeaveffiError>);

typedef ListNative = Pointer<Pointer<Void>> Function(
    Pointer<Size>, Pointer<WeaveffiError>);
typedef ListDart = Pointer<Pointer<Void>> Function(
    Pointer<Size>, Pointer<WeaveffiError>);

typedef GetIdNative = Int64 Function(Pointer<Void>);
typedef GetIdDart = int Function(Pointer<Void>);

typedef ListFreeNative = Void Function(Pointer<Pointer<Void>>, Size);
typedef ListFreeDart = void Function(Pointer<Pointer<Void>>, int);

typedef DeleteNative = Int32 Function(Uint64, Pointer<WeaveffiError>);
typedef DeleteDart = int Function(int, Pointer<WeaveffiError>);

typedef CountNative = Int32 Function(Pointer<WeaveffiError>);
typedef CountDart = int Function(Pointer<WeaveffiError>);

void check(bool cond, String msg) {
  if (!cond) {
    stderr.writeln('assertion failed: $msg');
    exit(1);
  }
}

void main() {
  final calcPath = Platform.environment['WEAVEFFI_LIB'];
  final contactsPath = Platform.environment['CONTACTS_LIB'];
  if (calcPath == null || contactsPath == null) {
    stderr.writeln('WEAVEFFI_LIB and CONTACTS_LIB must be set');
    exit(1);
  }

  final calc = DynamicLibrary.open(calcPath);
  final contacts = DynamicLibrary.open(contactsPath);

  final add =
      calc.lookupFunction<AddNative, AddDart>('weaveffi_calculator_add');
  final create = contacts
      .lookupFunction<CreateNative, CreateDart>('weaveffi_contacts_create_contact');
  final list = contacts
      .lookupFunction<ListNative, ListDart>('weaveffi_contacts_list_contacts');
  final getId = contacts
      .lookupFunction<GetIdNative, GetIdDart>('weaveffi_contacts_Contact_get_id');
  final listFree = contacts.lookupFunction<ListFreeNative, ListFreeDart>(
      'weaveffi_contacts_Contact_list_free');
  final del = contacts
      .lookupFunction<DeleteNative, DeleteDart>('weaveffi_contacts_delete_contact');
  final count = contacts
      .lookupFunction<CountNative, CountDart>('weaveffi_contacts_count_contacts');

  final err = calloc<WeaveffiError>();
  try {
    final sum = add(2, 3, err);
    check(err.ref.code == 0, 'calculator_add error');
    check(sum == 5, 'calculator_add(2,3) != 5');

    err.ref.code = 0;
    final fname = 'Alice'.toNativeUtf8();
    final lname = 'Smith'.toNativeUtf8();
    final email = 'alice@example.com'.toNativeUtf8();
    final h = create(fname, lname, email, 0, err);
    calloc.free(fname);
    calloc.free(lname);
    calloc.free(email);
    check(err.ref.code == 0, 'create_contact error');
    check(h != 0, 'create_contact returned 0');

    err.ref.code = 0;
    final lenPtr = calloc<Size>();
    final items = list(lenPtr, err);
    final len = lenPtr.value;
    calloc.free(lenPtr);
    check(err.ref.code == 0, 'list_contacts error');
    check(len == 1, 'list_contacts length != 1');
    check(getId(items[0]) == h, 'id mismatch');
    listFree(items, len);

    err.ref.code = 0;
    final deleted = del(h, err);
    check(err.ref.code == 0, 'delete_contact error');
    check(deleted == 1, 'delete_contact did not return 1');

    err.ref.code = 0;
    check(count(err) == 0, 'store not empty after cleanup');
  } finally {
    calloc.free(err);
  }

  print('OK');
}
