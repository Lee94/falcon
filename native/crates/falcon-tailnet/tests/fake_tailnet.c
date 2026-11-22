/*
 * 假的 libfalcon_tailnet：导出与 native/tailnet/main.go 同名同签名的符号，给 falcon-tailnet
 * 的加载与解析逻辑做单测，不起 Go 运行时、不连控制面。tests/fake_lib.rs 用系统 cc 现编。
 *
 *   -DFAKE_ABI=<n>              ftn_abi_version 返回 n（缺省 1）
 *   -DFAKE_NO_SET_INTERFACES    不导出 ftn_set_interfaces，测缺符号
 *
 * 另导出几个 fake_* 让测试看到 Rust 侧交进来的东西。
 */
#include <stdlib.h>
#include <string.h>

#ifdef _WIN32
#define EXPORT __declspec(dllexport)
#else
#define EXPORT __attribute__((visibility("default")))
#endif

#ifndef FAKE_ABI
#define FAKE_ABI 1
#endif

static char *last_config;
static char *last_interfaces;
static int running;
static int frees;

static char *dup_str(const char *s) {
    size_t n = strlen(s) + 1;
    char *p = malloc(n);
    memcpy(p, s, n);
    return p;
}

EXPORT int ftn_abi_version(void) { return FAKE_ABI; }

EXPORT char *ftn_start(char *cfg) {
    free(last_config);
    last_config = dup_str(cfg);
    if (strstr(cfg, "\"hostname\":\"fail\"")) {
        return dup_str("{\"error\":\"control plane unreachable\"}");
    }
    running = 1;
    return NULL;
}

EXPORT char *ftn_status(void) {
    if (!running) {
        return dup_str("{\"state\":\"stopped\",\"peers\":[]}");
    }
    return dup_str(
        "{\"state\":\"running\","
        "\"self\":{\"name\":\"falcon-test\",\"dnsName\":\"falcon-test.tail.ts.net\",\"ipv4\":\"100.64.0.9\",\"tailnet\":\"example.com\",\"magicDnsSuffix\":\"tail.ts.net\"},"
        "\"peers\":[{\"id\":\"na\",\"name\":\"work\",\"host\":\"work.tail.ts.net\",\"dnsName\":\"work.tail.ts.net\",\"ipv4\":\"100.64.0.2\",\"os\":\"linux\",\"online\":true,\"owner\":\"Ada\"},"
        "{\"id\":\"nb\",\"name\":\"nas\",\"host\":\"nas.tail.ts.net\",\"dnsName\":\"nas.tail.ts.net\",\"ipv4\":\"100.64.0.3\",\"online\":false}],"
        "\"future\":true}");
}

EXPORT char *ftn_proxy(void) {
    if (!running) {
        return NULL;
    }
    return dup_str("{\"addr\":\"127.0.0.1:41641\",\"user\":\"tsnet\",\"pass\":\"0123456789abcdef\"}");
}

EXPORT void ftn_stop(void) { running = 0; }

EXPORT char *ftn_logout(void) {
    running = 0;
    return dup_str("{\"error\":\"offline\"}");
}

#ifndef FAKE_NO_SET_INTERFACES
EXPORT char *ftn_set_interfaces(char *js) {
    free(last_interfaces);
    last_interfaces = dup_str(js);
    return NULL;
}
#endif

EXPORT void ftn_free(char *p) {
    if (p) {
        frees++;
        free(p);
    }
}

EXPORT const char *fake_last_config(void) { return last_config ? last_config : ""; }
EXPORT const char *fake_last_interfaces(void) { return last_interfaces ? last_interfaces : ""; }
EXPORT int fake_frees(void) { return frees; }
