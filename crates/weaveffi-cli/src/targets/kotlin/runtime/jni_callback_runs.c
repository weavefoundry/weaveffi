
/* Hands a string, bytes, or buffer return to the producer: a run allocated
   with {{PREFIX}}_alloc in the out slots, which the producer adopts. A NULL
   `value` (the shim reported a failure) leaves the slots empty. */
static void Jni_callback_return_bytes(JNIEnv* env, jbyteArray value, uint8_t** out_ptr, size_t* out_len, {{PREFIX}}_error* out_err) {
    if (value == NULL || (*env)->ExceptionCheck(env)) {
        return;
    }
    jsize len = (*env)->GetArrayLength(env, value);
    uint8_t* run = {{PREFIX}}_alloc((size_t)len);
    if (len > 0 && run == NULL) {
        {{PREFIX}}_error_set(out_err, -4, "out of memory");
        return;
    }
    if (len > 0) {
        (*env)->GetByteArrayRegion(env, value, 0, len, (jbyte*)run);
    }
    *out_ptr = run;
    *out_len = (size_t)len;
}
