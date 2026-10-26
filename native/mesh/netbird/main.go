// falcon-netbird：把 NetBird 的 client/embed 嵌进一个小进程，给 Rust 服务端当 sidecar。
//
// 与 falcon-tsnet 同一条边界：userspace 节点（netstack，不建 TUN、不要 root），
// stdout 一行 JSON。setup key 与 management URL 从环境变量读，不进 argv。
//
//	FALCON_NB_SETUP_KEY、FALCON_NB_MANAGEMENT_URL、FALCON_NB_HOSTNAME
//
// 状态目录是第一个参数。官方 embed API 自 v0.37.0 起随客户端发布（BSD-3 的客户端侧，
// 不是 AGPL 的 management/signal/relay）。
package main

import (
	"context"
	"encoding/json"
	"log"
	"os"
	"os/signal"
	"time"

	"github.com/netbirdio/netbird/client/embed"
)

type status struct {
	State    string `json:"state"`
	Hostname string `json:"hostname,omitempty"`
	IPv4     string `json:"ipv4,omitempty"`
	FQDN     string `json:"fqdn,omitempty"`
	Error    string `json:"error,omitempty"`
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
	host := os.Getenv("FALCON_NB_HOSTNAME")
	if host == "" {
		host = "falcon"
	}
	// netstack：无 TUN、无 root。空 StatePath 在这个模式下是纯内存，身份留不住，
	// 所以显式给一个文件。
	opts := embed.Options{
		DeviceName:    host,
		SetupKey:      os.Getenv("FALCON_NB_SETUP_KEY"),
		ManagementURL: os.Getenv("FALCON_NB_MANAGEMENT_URL"),
		StatePath:     dir + "/state.json",
	}
	client, err := embed.New(opts)
	if err != nil {
		emit(status{State: "error", Hostname: host, Error: err.Error()})
		os.Exit(1)
	}
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt)
	defer stop()
	emit(status{State: "starting", Hostname: host})
	if err := client.Start(ctx); err != nil {
		emit(status{State: "error", Hostname: host, Error: err.Error()})
		os.Exit(1)
	}
	defer client.Stop(context.Background())

	go report(ctx, client, host)
	<-ctx.Done()
}

func report(ctx context.Context, client *embed.Client, host string) {
	tick := time.NewTicker(2 * time.Second)
	defer tick.Stop()
	var last string
	for {
		st := snapshot(client, host)
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

func snapshot(client *embed.Client, host string) status {
	st := status{State: "starting", Hostname: host}
	full, err := client.Status()
	if err != nil {
		st.Error = err.Error()
		return st
	}
	local := full.LocalPeerState
	st.IPv4 = local.IP
	st.FQDN = local.FQDN
	if local.FQDN != "" {
		st.Hostname = local.FQDN
	}
	if full.ManagementState.Connected && local.IP != "" {
		st.State = "online"
	} else if full.ManagementState.Error != nil {
		st.State = "error"
		st.Error = full.ManagementState.Error.Error()
	}
	return st
}
