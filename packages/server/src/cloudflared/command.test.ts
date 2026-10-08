import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  CLOUDFLARED_VERSION,
  cloudflaredAsset,
  downloadUrl,
  extractMetricsAddr,
  extractQuickTunnelUrl,
  httpHostHeader,
  lastLogLines,
  originUrl,
  parseCloudflaredVersion,
  parseQuickTunnelMetrics,
  tunnelArgs,
} from "./command.js";

describe("cloudflaredAsset", () => {
  it("maps darwin/linux x64 to amd64 asset names", () => {
    assert.deepEqual(cloudflaredAsset("darwin", "arm64"), {
      name: "cloudflared-darwin-arm64.tgz",
      kind: "tgz",
    });
    assert.deepEqual(cloudflaredAsset("darwin", "x64"), {
      name: "cloudflared-darwin-amd64.tgz",
      kind: "tgz",
    });
    assert.deepEqual(cloudflaredAsset("linux", "x64"), {
      name: "cloudflared-linux-amd64",
      kind: "binary",
    });
    assert.deepEqual(cloudflaredAsset("linux", "arm64"), {
      name: "cloudflared-linux-arm64",
      kind: "binary",
    });
    assert.equal(cloudflaredAsset("linux", "ia32"), null);
  });
});

describe("downloadUrl", () => {
  it("pins the locked version and does not follow latest", () => {
    const asset = cloudflaredAsset("darwin", "arm64")!;
    assert.equal(
      downloadUrl(asset),
      `https://github.com/cloudflare/cloudflared/releases/download/${CLOUDFLARED_VERSION}/cloudflared-darwin-arm64.tgz`
    );
  });
});

describe("parseCloudflaredVersion", () => {
  it("reads the semver out of --version output", () => {
    assert.equal(
      parseCloudflaredVersion("cloudflared version 2026.10.0 (built 2026-10-05-17:37 UTC)"),
      "2026.10.0"
    );
    assert.equal(parseCloudflaredVersion("not a version"), null);
  });
});

describe("originUrl / httpHostHeader", () => {
  it("brackets IPv6 in the origin URL", () => {
    assert.equal(originUrl("::1", 80), "http://[::1]:80");
    assert.equal(originUrl("127.0.0.1", 5173), "http://127.0.0.1:5173");
  });

  it("rewrites loopback Host to localhost so vite allowedHosts accepts it", () => {
    assert.equal(httpHostHeader("127.0.0.1", 5173), "localhost:5173");
    assert.equal(httpHostHeader("::1", 80), "localhost:80");
    assert.equal(httpHostHeader("localhost", 3000), "localhost:3000");
    assert.equal(httpHostHeader("api.internal", 8080), "api.internal:8080");
  });
});

describe("tunnelArgs", () => {
  it("emits argv, never a shell string, and disables autoupdate", () => {
    assert.deepEqual(
      tunnelArgs({
        originUrl: "http://127.0.0.1:5173",
        httpHostHeader: "localhost:5173",
        metrics: "127.0.0.1:41234",
      }),
      [
        "tunnel",
        "--no-autoupdate",
        "--url",
        "http://127.0.0.1:5173",
        "--http-host-header",
        "localhost:5173",
        "--metrics",
        "127.0.0.1:41234",
      ]
    );
  });
});

describe("extractQuickTunnelUrl", () => {
  it("pulls the trycloudflare URL out of the ASCII box", () => {
    const log = `
INF Requesting new quick Tunnel on trycloudflare.com...
+--------------------------------------------------------------------------------------------+
|  Your quick Tunnel has been created! Visit it at (it may take some time to be reachable):  |
|  https://quiet-marble-otter.trycloudflare.com                                                    |
+--------------------------------------------------------------------------------------------+
`;
    assert.equal(extractQuickTunnelUrl(log), "https://quiet-marble-otter.trycloudflare.com");
  });

  it("ignores other https links so a docs URL is not treated as the tunnel", () => {
    assert.equal(extractQuickTunnelUrl("see https://developers.cloudflare.com/tunnel"), null);
  });
});

describe("extractMetricsAddr / parseQuickTunnelMetrics", () => {
  it("reads the metrics bind line", () => {
    assert.equal(
      extractMetricsAddr("INF Starting metrics server on 127.0.0.1:20241/metrics"),
      "127.0.0.1:20241"
    );
  });

  it("accepts hostname with or without https and rejects other domains", () => {
    assert.equal(
      parseQuickTunnelMetrics('{"hostname":"quiet-marble-otter.trycloudflare.com"}'),
      "https://quiet-marble-otter.trycloudflare.com"
    );
    assert.equal(
      parseQuickTunnelMetrics('{"hostname":"https://quiet-marble-otter.trycloudflare.com/"}'),
      "https://quiet-marble-otter.trycloudflare.com"
    );
    assert.equal(parseQuickTunnelMetrics('{"hostname":"evil.example"}'), null);
    assert.equal(parseQuickTunnelMetrics("not json"), null);
  });
});

describe("lastLogLines", () => {
  it("keeps the tail and drops blanks", () => {
    assert.equal(lastLogLines("a\n\nb\nc\nd\n", 3), "b · c · d");
  });
});
