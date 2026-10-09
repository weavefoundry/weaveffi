
/* Boxes: an optional scalar crossing out of native code is a nullable
   `java.lang.Integer` (and the like), and an async result or iterator item
   reaches Kotlin as an `Any?`, so scalars box through `valueOf` (which
   caches small values) and unbox through `intValue` (and the like). */
static jclass Jni_box_class[7];
static jmethodID Jni_box_of[7];
static jmethodID Jni_unbox_of[7];

static jint Jni_load_boxes(JNIEnv* env) {
    static const char* const classes[7] = {
        "java/lang/Boolean", "java/lang/Byte", "java/lang/Short", "java/lang/Integer",
        "java/lang/Long", "java/lang/Float", "java/lang/Double",
    };
    static const char* const value_of[7] = {
        "(Z)Ljava/lang/Boolean;", "(B)Ljava/lang/Byte;", "(S)Ljava/lang/Short;", "(I)Ljava/lang/Integer;",
        "(J)Ljava/lang/Long;", "(F)Ljava/lang/Float;", "(D)Ljava/lang/Double;",
    };
    static const char* const unbox[7] = {
        "booleanValue", "byteValue", "shortValue", "intValue", "longValue", "floatValue", "doubleValue",
    };
    static const char* const unbox_sig[7] = {"()Z", "()B", "()S", "()I", "()J", "()F", "()D"};
    for (int i = 0; i < 7; i++) {
        jclass cls = (*env)->FindClass(env, classes[i]);
        if (cls == NULL) {
            return JNI_ERR;
        }
        Jni_box_class[i] = (jclass)Jni_pin(env, cls);
        if (Jni_box_class[i] == NULL) {
            return JNI_ERR;
        }
        Jni_box_of[i] = (*env)->GetStaticMethodID(env, cls, "valueOf", value_of[i]);
        Jni_unbox_of[i] = (*env)->GetMethodID(env, cls, unbox[i], unbox_sig[i]);
        if (Jni_box_of[i] == NULL || Jni_unbox_of[i] == NULL) {
            return JNI_ERR;
        }
    }
    return JNI_OK;
}

/* The box of `value` (NULL with an exception pending if the JVM can't make
   one). */
Jni_helper jobject Jni_box(JNIEnv* env, Jni_kind kind, jvalue value) {
    return (*env)->CallStaticObjectMethodA(env, Jni_box_class[kind], Jni_box_of[kind], &value);
}

/* The scalar in the box `obj`. */
Jni_helper jvalue Jni_unbox(JNIEnv* env, Jni_kind kind, jobject obj) {
    jvalue v;
    memset(&v, 0, sizeof v);
    switch (kind) {
    case Jni_Z: v.z = (*env)->CallBooleanMethod(env, obj, Jni_unbox_of[kind]); break;
    case Jni_B: v.b = (*env)->CallByteMethod(env, obj, Jni_unbox_of[kind]); break;
    case Jni_S: v.s = (*env)->CallShortMethod(env, obj, Jni_unbox_of[kind]); break;
    case Jni_I: v.i = (*env)->CallIntMethod(env, obj, Jni_unbox_of[kind]); break;
    case Jni_J: v.j = (*env)->CallLongMethod(env, obj, Jni_unbox_of[kind]); break;
    case Jni_F: v.f = (*env)->CallFloatMethod(env, obj, Jni_unbox_of[kind]); break;
    case Jni_D: v.d = (*env)->CallDoubleMethod(env, obj, Jni_unbox_of[kind]); break;
    }
    return v;
}

/* Hands one iterator item (an object, or NULL for an absent optional) to
   Kotlin through the one-element array `out`; JNI_FALSE when making the item
   failed (the exception stays pending). */
Jni_helper jboolean Jni_yield(JNIEnv* env, jobjectArray out, jobject item) {
    if ((*env)->ExceptionCheck(env)) {
        return JNI_FALSE;
    }
    (*env)->SetObjectArrayElement(env, out, 0, item);
    return JNI_TRUE;
}
