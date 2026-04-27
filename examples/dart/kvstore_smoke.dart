// Kvstore consumer smoke test (Dart / dart:ffi).
//
// Loads KVSTORE_LIB at runtime via DynamicLibrary.open and exercises
// the minimum lifecycle every language binding must support: open
// store, put a value, get it back, delete it, close the store.
// Prints "OK" and exits 0 on success; any assertion failure exits 1.

import 'dart:ffi';
import 'dart:io';
import 'package:ffi/ffi.dart';

final class WeaveffiError extends Struct {
  @Int32()
  external int code;
  external Pointer<Utf8> message;
}

typedef OpenNative = Pointer<Void> Function(Pointer<Utf8>, Pointer<WeaveffiError>);
typedef OpenDart = Pointer<Void> Function(Pointer<Utf8>, Pointer<WeaveffiError>);

typedef CloseNative = Void Function(Pointer<Void>, Pointer<WeaveffiError>);
typedef CloseDart = void Function(Pointer<Void>, Pointer<WeaveffiError>);

typedef PutNative = Bool Function(
    Pointer<Void>, Pointer<Utf8>, Pointer<Uint8>, Size, Int32,
    Pointer<Int64>, Pointer<WeaveffiError>);
typedef PutDart = bool Function(
    Pointer<Void>, Pointer<Utf8>, Pointer<Uint8>, int, int,
    Pointer<Int64>, Pointer<WeaveffiError>);

typedef GetNative = Pointer<Void> Function(
    Pointer<Void>, Pointer<Utf8>, Pointer<WeaveffiError>);
typedef GetDart = Pointer<Void> Function(
    Pointer<Void>, Pointer<Utf8>, Pointer<WeaveffiError>);

typedef EntryValueNative = Pointer<Uint8> Function(Pointer<Void>, Pointer<Size>);
typedef EntryValueDart = Pointer<Uint8> Function(Pointer<Void>, Pointer<Size>);

typedef EntryDestroyNative = Void Function(Pointer<Void>);
typedef EntryDestroyDart = void Function(Pointer<Void>);

typedef DeleteNative = Bool Function(
    Pointer<Void>, Pointer<Utf8>, Pointer<WeaveffiError>);
typedef DeleteDart = bool Function(
    Pointer<Void>, Pointer<Utf8>, Pointer<WeaveffiError>);

typedef FreeBytesNative = Void Function(Pointer<Uint8>, Size);
typedef FreeBytesDart = void Function(Pointer<Uint8>, int);

void check(bool cond, String msg) {
  if (!cond) {
    stderr.writeln('assertion failed: $msg');
    exit(1);
  }
}

void main() {
  final kvPath = Platform.environment['KVSTORE_LIB'];
  if (kvPath == null) {
    stderr.writeln('KVSTORE_LIB must be set');
    exit(1);
  }

  final kv = DynamicLibrary.open(kvPath);

  final openStore =
      kv.lookupFunction<OpenNative, OpenDart>('weaveffi_kv_open_store');
  final closeStore =
      kv.lookupFunction<CloseNative, CloseDart>('weaveffi_kv_close_store');
  final put = kv.lookupFunction<PutNative, PutDart>('weaveffi_kv_put');
  final get = kv.lookupFunction<GetNative, GetDart>('weaveffi_kv_get');
  final entryValue = kv.lookupFunction<EntryValueNative, EntryValueDart>(
      'weaveffi_kv_Entry_get_value');
  final entryDestroy = kv.lookupFunction<EntryDestroyNative, EntryDestroyDart>(
      'weaveffi_kv_Entry_destroy');
  final del = kv.lookupFunction<DeleteNative, DeleteDart>('weaveffi_kv_delete');
  final freeBytes =
      kv.lookupFunction<FreeBytesNative, FreeBytesDart>('weaveffi_free_bytes');

  final err = calloc<WeaveffiError>();
  try {
    final path = '/tmp/kvstore-dart-smoke'.toNativeUtf8();
    final store = openStore(path, err);
    calloc.free(path);
    check(err.ref.code == 0, 'open_store error');
    check(store != nullptr, 'open_store returned null');

    err.ref.code = 0;
    final key = 'greeting'.toNativeUtf8();
    final value = calloc<Uint8>(5);
    final bytes = [104, 101, 108, 108, 111];
    for (var i = 0; i < 5; i++) {
      value[i] = bytes[i];
    }
    final ok = put(store, key, value, 5, 1, nullptr, err);
    calloc.free(value);
    check(err.ref.code == 0, 'put error');
    check(ok, 'put returned false');

    err.ref.code = 0;
    final entry = get(store, key, err);
    check(err.ref.code == 0, 'get error');
    check(entry != nullptr, 'get returned null');

    final lenPtr = calloc<Size>();
    final got = entryValue(entry, lenPtr);
    final len = lenPtr.value;
    calloc.free(lenPtr);
    check(len == 5, 'value length mismatch');
    for (var i = 0; i < 5; i++) {
      check(got[i] == bytes[i], 'value bytes mismatch at $i');
    }
    freeBytes(got, len);
    entryDestroy(entry);

    err.ref.code = 0;
    final deleted = del(store, key, err);
    calloc.free(key);
    check(err.ref.code == 0, 'delete error');
    check(deleted, 'delete did not return true');

    err.ref.code = 0;
    closeStore(store, err);
    check(err.ref.code == 0, 'close_store error');
  } finally {
    calloc.free(err);
  }

  print('OK');
}
