/*
 * cadence_seal — the reviewed native bootstrap loaded by the pinned observer
 * *inside* the measured Node process, after exec.
 *
 * Linux resets mm->dumpable to SUID_DUMP_USER on execve() for a normal
 * credential-preserving exec, so a pre-exec prctl cannot survive. This addon is
 * loaded by the first line of the observer extension and re-applies the
 * post-exec seal, then verifies it with the kernel:
 *
 *   - prctl(PR_SET_DUMPABLE, 0) and PR_GET_DUMPABLE == 0
 *   - setrlimit(RLIMIT_CORE, 0) and getrlimit soft == hard == 0
 *   - prctl(PR_SET_NO_NEW_PRIVS, 1) and PR_GET_NO_NEW_PRIVS == 1
 *   - /proc/self/environ is owned by root with no group/other bits (the
 *     kernel-visible proof of non-dumpability for a non-root uid)
 *
 * It exposes exactly one function, `seal()`, returning the verified facts.
 * There is no sign/eval/key-getter/exec surface.
 */
#include <node_api.h>

#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <unistd.h>

static int environ_owner_proof(void) {
    struct stat st;
    if (stat("/proc/self/environ", &st) != 0) {
        return 0;
    }
    return st.st_uid == 0 && (st.st_mode & 077) == 0;
}

static int apply_seal(long *dumpable, long *core_soft, long *nnp) {
    if (prctl(PR_SET_DUMPABLE, 0, 0, 0, 0) != 0) {
        return -1;
    }
    *dumpable = prctl(PR_GET_DUMPABLE, 0, 0, 0, 0);
    if (*dumpable != 0) {
        return -1;
    }
    struct rlimit limit;
    limit.rlim_cur = 0;
    limit.rlim_max = 0;
    if (setrlimit(RLIMIT_CORE, &limit) != 0) {
        return -1;
    }
    struct rlimit check;
    if (getrlimit(RLIMIT_CORE, &check) != 0) {
        return -1;
    }
    *core_soft = (long)check.rlim_cur;
    if (check.rlim_cur != 0 || check.rlim_max != 0) {
        return -1;
    }
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0) {
        return -1;
    }
    *nnp = prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0);
    if (*nnp != 1) {
        return -1;
    }
    if (!environ_owner_proof()) {
        return -1;
    }
    return 0;
}

static napi_value set_int(napi_env env, napi_value object, const char *name, long value) {
    napi_value number;
    if (napi_create_int64(env, (int64_t)value, &number) != napi_ok) {
        return NULL;
    }
    if (napi_set_named_property(env, object, name, number) != napi_ok) {
        return NULL;
    }
    return number;
}

static napi_value seal(napi_env env, napi_callback_info info) {
    (void)info;
    long dumpable = -1;
    long core_soft = -1;
    long nnp = -1;
    if (apply_seal(&dumpable, &core_soft, &nnp) != 0) {
        napi_throw_error(env, "CADENCE_SEAL", "post-exec seal verification failed");
        return NULL;
    }
    napi_value result;
    if (napi_create_object(env, &result) != napi_ok) {
        return NULL;
    }
    if (set_int(env, result, "dumpable", dumpable) == NULL ||
        set_int(env, result, "core", core_soft) == NULL ||
        set_int(env, result, "nnp", nnp) == NULL) {
        return NULL;
    }
    napi_value uid;
    if (napi_create_int64(env, (int64_t)getuid(), &uid) != napi_ok ||
        napi_set_named_property(env, result, "uid", uid) != napi_ok) {
        return NULL;
    }
    return result;
}

NAPI_MODULE_INIT() {
    napi_value fn;
    if (napi_create_function(env, "seal", NAPI_AUTO_LENGTH, seal, NULL, &fn) != napi_ok) {
        return NULL;
    }
    if (napi_set_named_property(env, exports, "seal", fn) != napi_ok) {
        return NULL;
    }
    return exports;
}
