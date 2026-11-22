package main

import (
	"context"
	"fmt"
	"net"
	"sync"
	"time"

	"tailscale.com/net/netmon"
)

// 安卓（targetSdk ≥ 30）不让 App 用 netlink 枚举网卡，Go 的 net.Interfaces() 会报错，
// netmon 就判断不了网络变化。照官方安卓 App 的做法，由 Kotlin 从 ConnectivityManager
// 读网卡与 DNS，经 JNI 交进来（ADR 0030 决定六）。网络切换时再交一次。

type ifaceJSON struct {
	Name  string   `json:"name"`
	Index int      `json:"index"`
	MTU   int      `json:"mtu"`
	Up    bool     `json:"up"`
	Addrs []string `json:"addrs"` // CIDR，例如 192.168.1.5/24、fe80::1/64
}

type interfaceList struct {
	Interfaces []ifaceJSON `json:"interfaces"`
	DNS        []string    `json:"dns"` // DNS 服务器 IP
}

var (
	ifMu       sync.Mutex
	ifCurrent  []netmon.Interface
	ifRegister sync.Once
)

func setInterfaces(l interfaceList) error {
	list := make([]netmon.Interface, 0, len(l.Interfaces))
	for _, it := range l.Interfaces {
		var flags net.Flags
		if it.Up {
			flags = net.FlagUp | net.FlagRunning | net.FlagMulticast
		}
		addrs := make([]net.Addr, 0, len(it.Addrs))
		for _, a := range it.Addrs {
			ip, ipnet, err := net.ParseCIDR(a)
			if err != nil {
				return fmt.Errorf("网卡 %s 的地址 %q：%w", it.Name, a, err)
			}
			addrs = append(addrs, &net.IPNet{IP: ip, Mask: ipnet.Mask})
		}
		list = append(list, netmon.Interface{
			Interface: &net.Interface{Index: it.Index, MTU: it.MTU, Name: it.Name, Flags: flags},
			AltAddrs:  addrs,
		})
	}
	servers := make([]string, 0, len(l.DNS))
	for _, s := range l.DNS {
		ip := net.ParseIP(s)
		if ip == nil {
			return fmt.Errorf("DNS 服务器 %q 不是 IP", s)
		}
		servers = append(servers, net.JoinHostPort(ip.String(), "53"))
	}

	ifMu.Lock()
	ifCurrent = list
	ifMu.Unlock()
	ifRegister.Do(func() {
		netmon.RegisterInterfaceGetter(func() ([]netmon.Interface, error) {
			ifMu.Lock()
			defer ifMu.Unlock()
			return append([]netmon.Interface(nil), ifCurrent...), nil
		})
	})

	// 安卓没有 /etc/resolv.conf，Go 的纯 Go 解析器找不到上游。tailnet 里的名字由 tsnet
	// 自己解析，这里管的是控制面、DERP 之类的公网名字
	if len(servers) > 0 {
		net.DefaultResolver = &net.Resolver{
			PreferGo: true,
			Dial: func(ctx context.Context, network, _ string) (net.Conn, error) {
				d := net.Dialer{Timeout: 3 * time.Second}
				var lastErr error
				for _, s := range servers {
					c, err := d.DialContext(ctx, network, s)
					if err == nil {
						return c, nil
					}
					lastErr = err
				}
				return nil, lastErr
			},
		}
	}
	return nil
}
