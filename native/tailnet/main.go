// libfalcon_tailnet：Falcon 客户端内置的 Tailscale 节点（ADR 0030）。
//
// 进程内嵌 tsnet 当 userspace 节点，编成 c-shared 动态库，由 Rust 包装 crate
// falcon-tailnet 用 dlopen 加载。导出面尽量小：参数与返回值都是 UTF-8 JSON 字符串，
// 返回的非 NULL 字符串一律由调用方交给 ftn_free 释放。不做 Go 调 C 的回调，客户端轮询
// ftn_status。
//
// 改了导出签名或 JSON 形状的不兼容变化就把 abiVersion 加一；Rust 侧对不上就不用这份库。
package main

/*
#include <stdlib.h>
*/
import "C"

import (
	"encoding/json"
	"unsafe"
)

const abiVersion = 1

func main() {} // c-shared 要求有 main，不会执行

// cstr 把 Go 字符串交给 C（malloc 出来的），调用方用 ftn_free 释放
func cstr(s string) *C.char { return C.CString(s) }

func cjson(v any) *C.char {
	b, err := json.Marshal(v)
	if err != nil {
		b, _ = json.Marshal(map[string]string{"state": "error", "error": err.Error()})
	}
	return cstr(string(b))
}

func errJSON(err error) *C.char {
	if err == nil {
		return nil
	}
	return cjson(map[string]string{"error": err.Error()})
}

//export ftn_abi_version
func ftn_abi_version() C.int { return abiVersion }

// ftn_start 起节点；本进程已有节点时就是重启。cfg：{"stateDir","hostname","controlUrl"?,"authKey"?}。
// 成功返回 NULL，否则 {"error": "..."}。
//
//export ftn_start
func ftn_start(cfg *C.char) *C.char {
	var c startConfig
	if err := json.Unmarshal([]byte(C.GoString(cfg)), &c); err != nil {
		return errJSON(err)
	}
	return errJSON(node.start(c))
}

// ftn_status 总是返回 JSON，形状见 nodeStatus
//
//export ftn_status
func ftn_status() *C.char { return cjson(node.status()) }

// ftn_proxy 返回回环 SOCKS5 代理 {"addr","user","pass"}；节点不在 running 时返回 NULL
//
//export ftn_proxy
func ftn_proxy() *C.char {
	p, ok := node.proxy()
	if !ok {
		return nil
	}
	return cjson(p)
}

// ftn_stop 停掉节点，保留身份
//
//export ftn_stop
func ftn_stop() { node.stop() }

// ftn_logout 向控制面退出登录并停掉节点。状态目录由调用方清空（它知道目录在哪，节点
// 没起过也能清）。返回 NULL 或 {"error"}
//
//export ftn_logout
func ftn_logout() *C.char { return errJSON(node.logout()) }

// ftn_set_interfaces 仅安卓用：App 从 ConnectivityManager 读到的网卡与 DNS 交进来
// （见 interfaces.go）。返回 NULL 或 {"error"}
//
//export ftn_set_interfaces
func ftn_set_interfaces(js *C.char) *C.char {
	var l interfaceList
	if err := json.Unmarshal([]byte(C.GoString(js)), &l); err != nil {
		return errJSON(err)
	}
	return errJSON(setInterfaces(l))
}

//export ftn_free
func ftn_free(p *C.char) {
	if p != nil {
		C.free(unsafe.Pointer(p))
	}
}
