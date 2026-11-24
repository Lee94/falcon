package main

import (
	"context"
	"errors"
	"net/netip"
	"os"
	"runtime"
	"sort"
	"strings"
	"sync"
	"time"

	"tailscale.com/ipn/ipnstate"
	"tailscale.com/tsnet"
)

type startConfig struct {
	StateDir   string `json:"stateDir"`
	Hostname   string `json:"hostname"`
	ControlURL string `json:"controlUrl,omitempty"`
	AuthKey    string `json:"authKey,omitempty"`
}

// 状态值，与 Rust 侧 falcon_tailnet::State 一一对应
const (
	stateStopped       = "stopped"
	stateStarting      = "starting"
	stateNeedsLogin    = "needs_login"
	stateNeedsApproval = "needs_approval" // 登录了，等 tailnet 管理员批准这台设备
	stateRunning       = "running"
	stateError         = "error"
)

type selfNode struct {
	Name           string `json:"name"`
	DNSName        string `json:"dnsName,omitempty"`
	IPv4           string `json:"ipv4,omitempty"`
	IPv6           string `json:"ipv6,omitempty"`
	Tailnet        string `json:"tailnet,omitempty"`
	MagicDNSSuffix string `json:"magicDnsSuffix,omitempty"`
}

// device 与 falcon_proto::TailscaleDevice 同形（camelCase、可选字段省略）
type device struct {
	ID      string `json:"id"`
	Name    string `json:"name"`
	Host    string `json:"host"`
	DNSName string `json:"dnsName,omitempty"`
	IPv4    string `json:"ipv4,omitempty"`
	OS      string `json:"os,omitempty"`
	Online  bool   `json:"online"`
	Owner   string `json:"owner,omitempty"`
}

type nodeStatus struct {
	State    string    `json:"state"`
	LoginURL string    `json:"loginUrl,omitempty"`
	Self     *selfNode `json:"self,omitempty"`
	Peers    []device  `json:"peers"`
	Error    string    `json:"error,omitempty"`
}

type proxyInfo struct {
	Addr string `json:"addr"`
	User string `json:"user"`
	Pass string `json:"pass"`
}

// 一个进程最多一个节点（Go 共享库卸载不了，断开只 Close，见 ADR 0030 决定一）
type nodeState struct {
	mu    sync.Mutex
	srv   *tsnet.Server
	err   string     // 最近一次起节点的错误；Close 或重新起时清掉
	prox  *proxyInfo // Loopback() 的结果，节点换了就作废
}

var node nodeState

func discard(string, ...any) {}

func (n *nodeState) closeLocked() {
	if n.srv != nil {
		n.srv.Close()
	}
	n.srv, n.prox = nil, nil
}

func (n *nodeState) start(c startConfig) error {
	n.mu.Lock()
	defer n.mu.Unlock()
	n.closeLocked()
	n.err = ""
	fail := func(err error) error {
		n.err = err.Error()
		return err
	}
	if c.StateDir == "" {
		return fail(errors.New("stateDir 为空"))
	}
	if err := os.MkdirAll(c.StateDir, 0o700); err != nil {
		return fail(err)
	}
	if err := os.Chmod(c.StateDir, 0o700); err != nil {
		return fail(err)
	}
	if runtime.GOOS == "android" {
		// tsnet.Dir 只指定节点状态目录。Android 没有可用的默认日志目录，
		// 必须在 Start 前指定已创建的私有目录，否则 logpolicy.LogsDir 会 panic。
		if err := os.Setenv("TS_LOGS_DIR", c.StateDir); err != nil {
			return fail(err)
		}
	}
	s := &tsnet.Server{
		Dir:        c.StateDir,
		Hostname:   c.Hostname,
		ControlURL: c.ControlURL,
		AuthKey:    c.AuthKey,
		Ephemeral:  false,
		// tsnet 的日志很吵，UserLogf 还会把带 query 的登录 URL 打出来。登录 URL 从
		// Status().AuthURL 取，日志一律丢掉
		Logf:     discard,
		UserLogf: discard,
	}
	// Start 不等登录：控制面握手与登录都在后台，进度由 status 反映
	if err := s.Start(); err != nil {
		s.Close()
		return fail(err)
	}
	// auth key 只在 Start 里读一次，之后不再持有（ADR 0030「安全」）
	s.AuthKey = ""
	n.srv = s
	return nil
}

func (n *nodeState) stop() {
	n.mu.Lock()
	defer n.mu.Unlock()
	n.closeLocked()
	n.err = ""
}

func (n *nodeState) logout() error {
	n.mu.Lock()
	defer n.mu.Unlock()
	defer func() {
		n.closeLocked()
		n.err = ""
	}()
	if n.srv == nil {
		return nil
	}
	lc, err := n.srv.LocalClient()
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	return lc.Logout(ctx)
}

func (n *nodeState) rawStatus() (*tsnet.Server, *ipnstate.Status, string) {
	n.mu.Lock()
	s, startErr := n.srv, n.err
	n.mu.Unlock()
	if s == nil {
		return nil, nil, startErr
	}
	lc, err := s.LocalClient()
	if err != nil {
		return s, nil, err.Error()
	}
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	st, err := lc.Status(ctx)
	if err != nil {
		return s, nil, err.Error()
	}
	return s, st, ""
}

func (n *nodeState) status() nodeStatus {
	s, st, errText := n.rawStatus()
	out := nodeStatus{State: stateStopped, Peers: []device{}}
	switch {
	case s == nil && errText != "":
		out.State, out.Error = stateError, errText
		return out
	case s == nil:
		return out
	case st == nil:
		out.State, out.Error = stateError, errText
		return out
	}
	switch st.BackendState {
	case "Running":
		out.State = stateRunning
	case "NeedsLogin":
		out.State = stateNeedsLogin
		out.LoginURL = st.AuthURL
	case "NeedsMachineAuth":
		out.State = stateNeedsApproval
	case "Stopped":
		out.State = stateStopped
	default: // NoState / Starting
		out.State = stateStarting
	}
	magicDNS := true
	if st.CurrentTailnet != nil {
		magicDNS = st.CurrentTailnet.MagicDNSEnabled
	}
	if st.Self != nil && out.State == stateRunning {
		v4, v6 := splitIPs(st.Self.TailscaleIPs)
		self := &selfNode{
			Name:           st.Self.HostName,
			DNSName:        trimDNS(st.Self.DNSName),
			IPv4:           v4,
			IPv6:           v6,
			MagicDNSSuffix: st.MagicDNSSuffix,
		}
		if st.CurrentTailnet != nil {
			self.Tailnet = st.CurrentTailnet.Name
		}
		out.Self = self
	}
	for _, p := range st.Peer {
		if p == nil {
			continue
		}
		if d, ok := toDevice(p, magicDNS, st); ok {
			out.Peers = append(out.Peers, d)
		}
	}
	// 顺序固定，Rust 侧按整份状态比较「有没有变化」
	sort.Slice(out.Peers, func(i, j int) bool {
		a, b := out.Peers[i], out.Peers[j]
		if a.Online != b.Online {
			return a.Online
		}
		if la, lb := strings.ToLower(a.Name), strings.ToLower(b.Name); la != lb {
			return la < lb
		}
		return a.ID < b.ID
	})
	return out
}

// toDevice 与服务端 falcon-server/src/tailscale/command.rs 的 into_device 同一套口径：
// MagicDNS 开着用 DNS 名当地址，关着用 Tailscale IPv4；名字缺省依次退到 DNS 首段、IPv4、id
func toDevice(p *ipnstate.PeerStatus, magicDNS bool, st *ipnstate.Status) (device, bool) {
	dns := trimDNS(p.DNSName)
	v4, v6 := splitIPs(p.TailscaleIPs)
	host := firstNonEmpty(dns, v4, v6)
	if !magicDNS {
		host = firstNonEmpty(v4, dns, v6)
	}
	if host == "" {
		return device{}, false
	}
	id := string(p.ID)
	name := p.HostName
	if name == "" {
		if label, _, _ := strings.Cut(dns, "."); label != "" {
			name = label
		} else {
			name = firstNonEmpty(v4, id, "tailscale")
		}
	}
	d := device{ID: id, Name: name, Host: host, DNSName: dns, IPv4: v4, OS: p.OS, Online: p.Online}
	if u, ok := st.User[p.UserID]; ok {
		d.Owner = firstNonEmpty(u.DisplayName, u.LoginName)
	}
	return d, true
}

func (n *nodeState) proxy() (proxyInfo, bool) {
	s, st, _ := n.rawStatus()
	if s == nil || st == nil || st.BackendState != "Running" {
		return proxyInfo{}, false
	}
	n.mu.Lock()
	defer n.mu.Unlock()
	if n.srv != s { // 查状态期间节点换了
		return proxyInfo{}, false
	}
	if n.prox == nil {
		// 127.0.0.1 随机端口上的 SOCKS5（另带一个 LocalAPI，我们不用）。用户名固定是
		// tsnet，口令每次起节点随机生成，只在内存里
		addr, proxyCred, _, err := s.Loopback()
		if err != nil {
			return proxyInfo{}, false
		}
		n.prox = &proxyInfo{Addr: addr, User: "tsnet", Pass: proxyCred}
	}
	return *n.prox, true
}

func splitIPs(ips []netip.Addr) (v4, v6 string) {
	for _, ip := range ips {
		switch {
		case ip.Is4() && v4 == "":
			v4 = ip.String()
		case ip.Is6() && v6 == "":
			v6 = ip.String()
		}
	}
	return
}

func trimDNS(s string) string { return strings.TrimSpace(strings.TrimSuffix(strings.TrimSpace(s), ".")) }

func firstNonEmpty(vals ...string) string {
	for _, v := range vals {
		if v != "" {
			return v
		}
	}
	return ""
}
