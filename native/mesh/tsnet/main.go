// falcon-tsnet：把 Tailscale 的 tsnet 嵌进一个小进程，给 Rust 服务端当 sidecar。
//
// Falcon 是 Rust，tsnet 是 Go 库，链不进去。这个进程就是 Rex `--tailscale` 的对应物：
// userspace 节点，不必另装 tailscaled，也不要 root。stdout 上只打一行 JSON 状态；
// 登录 URL 走日志（第一次没有 auth key 时 tsnet 自己打出来）。
//
// 密钥与控制面地址从环境变量读，不进 argv，免得落进进程列表：
//
//	FALCON_TSNET_AUTHKEY、FALCON_TSNET_CONTROL_URL（Headscale）、FALCON_TSNET_HOSTNAME
//
// 状态目录是第一个参数，节点身份存在那里，重启不用重新登录。
package main

import (
	"context"
	"encoding/json"
	"log"
	"os"
	"os/signal"
	"time"

	"tailscale.com/tsnet"
)

type status struct {
	State    string   `json:"state"`
	Hostname string   `json:"hostname,omitempty"`
	IPv4     string   `json:"ipv4,omitempty"`
	IPv6     string   `json:"ipv6,omitempty"`
	DNSName  string   `json:"dnsName,omitempty"`
	Error    string   `json:"error,omitempty"`
	Domains  []string `json:"domains,omitempty"`
}

func emit(s status) {
	b, err := json.Marshal(s)
	if err != nil {
		return
	}
	os.Stdout.Write(append(b, '\n'))
}

func main() {
	log.SetOutput(os.Stderr)
	if len(os.Args) < 2 || os.Args[1] == "" {
		emit(status{State: "error", Error: "需要状态目录参数"})
		os.Exit(2)
	}
	dir := os.Args[1]
	if err := os.MkdirAll(dir, 0o700); err != nil {
		emit(status{State: "error", Error: err.Error()})
		os.Exit(1)
	}
	host := os.Getenv("FALCON_TSNET_HOSTNAME")
	if host == "" {
		host = "falcon"
	}
	s := &tsnet.Server{
		Dir:        dir,
		Hostname:   host,
		AuthKey:    os.Getenv("FALCON_TSNET_AUTHKEY"),
		ControlURL: os.Getenv("FALCON_TSNET_CONTROL_URL"),
		Ephemeral:  false,
	}
	defer s.Close()

	emit(status{State: "starting", Hostname: host})
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt)
	defer stop()

	if err := s.Start(); err != nil {
		emit(status{State: "error", Hostname: host, Error: err.Error()})
		os.Exit(1)
	}

	go report(ctx, s, host)
	<-ctx.Done()
}

func report(ctx context.Context, s *tsnet.Server, host string) {
	tick := time.NewTicker(2 * time.Second)
	defer tick.Stop()
	var last string
	for {
		st := snapshot(s, host)
		b, _ := json.Marshal(st)
		if string(b) != last {
			last = string(b)
			os.Stdout.Write(append(b, '\n'))
		}
		select {
		case <-ctx.Done():
			return
		case <-tick.C:
		}
	}
}

func snapshot(s *tsnet.Server, host string) status {
	ip4, ip6 := s.TailscaleIPs()
	st := status{State: "offline", Hostname: host}
	if ip4.IsValid() || ip6.IsValid() {
		st.State = "online"
	}
	if ip4.IsValid() {
		st.IPv4 = ip4.String()
	}
	if ip6.IsValid() {
		st.IPv6 = ip6.String()
	}
	if domains := s.CertDomains(); len(domains) > 0 {
		st.Domains = domains
		st.DNSName = domains[0]
	}
	if st.IPv4 == "" && st.IPv6 == "" {
		// 还没拿到地址：多半卡在第一次登录。tsnet 把登录 URL 打在 stderr。
		st.State = "needs-login"
	}
	return st
}
