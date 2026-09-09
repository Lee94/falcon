import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  classifyDockerFailure,
  clampTail,
  composeArgs,
  composeDownArgs,
  composeUpArgs,
  COMPOSE_SKIP_DIRS,
  errorDetail,
  findComposeFilesCommand,
  imagePruneArgs,
  inferContainerState,
  isComposeFileName,
  isContainerRunning,
  isSafeComposeRel,
  isSafeDockerRef,
  logsArgs,
  parseComposeFind,
  parseComposePs,
  parseDockerOpInput,
  parseImagesJson,
  parseJsonLines,
  parsePsJson,
  psArgs,
  rmArgs,
  rmiArgs,
  startArgs,
  stopArgs,
  truncateLogs,
  LOGS_DEFAULT_TAIL,
  LOGS_MAX_BYTES,
} from "./command.js";

describe("isSafeDockerRef", () => {
  it("accepts ids, names, tags, digests, registry paths", () => {
    for (const s of [
      "abc123def456",
      "web",
      "my_app.1",
      "nginx:latest",
      "ghcr.io/foo/bar:v1.2",
      "sha256:0123456789abcdef0123456789abcdef",
      "nginx@sha256:0123456789abcdef",
    ]) {
      assert.equal(isSafeDockerRef(s), true, s);
    }
  });

  it("rejects flags, paths, shell metacharacters, empty", () => {
    for (const s of [
      "",
      "-f",
      "--privileged",
      "../etc/passwd",
      "a b",
      "a;rm",
      "a$(x)",
      "a`x`",
      "a\nb",
      "/var/run/docker.sock",
    ]) {
      assert.equal(isSafeDockerRef(s), false, s);
    }
  });
});

describe("isSafeComposeRel / isComposeFileName", () => {
  it("accepts known filenames at reasonable depth", () => {
    assert.equal(isComposeFileName("compose.yaml"), true);
    assert.equal(isComposeFileName("docker-compose.yml"), true);
    assert.equal(isComposeFileName("Compose.yaml"), true);
    assert.equal(isSafeComposeRel("compose.yaml"), true);
    assert.equal(isSafeComposeRel("deploy/compose.yml"), true);
    assert.equal(isSafeComposeRel("a/b/docker-compose.yaml"), true);
  });

  it("rejects traversal, absolute, wrong basename, too deep", () => {
    assert.equal(isSafeComposeRel("../compose.yaml"), false);
    assert.equal(isSafeComposeRel("/etc/compose.yaml"), false);
    assert.equal(isSafeComposeRel("compose.json"), false);
    assert.equal(isSafeComposeRel("a/b/c/d/compose.yaml"), false);
    assert.equal(isSafeComposeRel("x.txt"), false);
    assert.equal(isComposeFileName("Dockerfile"), false);
  });
});

describe("argv construction never interpolates into a shell string", () => {
  const docker = "/usr/bin/docker";
  const nasty = "web;rm -rf /";

  it("ps / logs / start / stop / rm keep ref as its own argv slot", () => {
    assert.deepEqual(psArgs(docker).slice(0, 3), [docker, "ps", "-a"]);
    assert.equal(psArgs(docker).includes("{{json .}}"), true);
    assert.deepEqual(startArgs(docker, nasty), [docker, "start", nasty]);
    assert.deepEqual(stopArgs(docker, nasty), [docker, "stop", nasty]);
    assert.deepEqual(rmArgs(docker, nasty, true), [docker, "rm", "-f", nasty]);
    assert.deepEqual(rmArgs(docker, nasty, false), [docker, "rm", nasty]);
    assert.deepEqual(logsArgs(docker, nasty, 50).slice(-1), [nasty]);
    assert.deepEqual(rmiArgs(docker, "sha256:abc"), [docker, "rmi", "sha256:abc"]);
    assert.deepEqual(imagePruneArgs(docker), [docker, "image", "prune", "-a", "-f"]);
  });

  it("compose plugin vs standalone share -f and --project-directory", () => {
    const file = "/home/u/app/compose.yaml";
    const dir = "/home/u/app";
    assert.deepEqual(composeArgs(docker, { kind: "plugin" }, file, dir, "up", "-d"), [
      docker,
      "compose",
      "-f",
      file,
      "--project-directory",
      dir,
      "up",
      "-d",
    ]);
    assert.deepEqual(
      composeUpArgs(docker, { kind: "standalone", bin: "/usr/bin/docker-compose" }, file, dir),
      ["/usr/bin/docker-compose", "-f", file, "--project-directory", dir, "up", "-d"]
    );
    assert.equal(composeDownArgs(docker, { kind: "plugin" }, file, dir).includes("down"), true);
    assert.equal(composeDownArgs(docker, { kind: "plugin" }, file, dir).includes("-v"), false);
  });
});

describe("parsePsJson", () => {
  it("reads NDJSON and infers state from Status when State is missing", () => {
    const stdout = [
      `{"ID":"abc123","Names":"web","Image":"nginx:latest","Status":"Up 2 hours","Ports":"0.0.0.0:8080->80/tcp","CreatedAt":"2024-01-01","Command":"\\"nginx -g 'daemon off;'\\""}`,
      `{"ID":"def456","Names":"/db,/db-alias","Image":"postgres:16","State":"exited","Status":"Exited (0) 3 days ago","Ports":"","CreatedAt":"2024-01-02","Command":"postgres"}`,
    ].join("\n");
    const rows = parsePsJson(stdout);
    assert.equal(rows.length, 2);
    assert.equal(rows[0]!.id, "abc123");
    assert.deepEqual(rows[0]!.names, ["web"]);
    assert.equal(rows[0]!.state, "running");
    assert.equal(rows[0]!.ports, "0.0.0.0:8080->80/tcp");
    assert.deepEqual(rows[1]!.names, ["db", "db-alias"]);
    assert.equal(rows[1]!.state, "exited");
  });

  it("skips warning lines mixed into stdout", () => {
    const stdout = `WARNING: human readable\n{"ID":"x","Names":"a","Image":"b","Status":"Up"}\n`;
    assert.equal(parsePsJson(stdout).length, 1);
  });
});

describe("parseImagesJson", () => {
  it("marks <none> as dangling", () => {
    const stdout = [
      `{"ID":"sha256:aaa","Repository":"nginx","Tag":"latest","Size":"187MB","CreatedSince":"2 weeks ago"}`,
      `{"ID":"sha256:bbb","Repository":"<none>","Tag":"<none>","Size":"12MB","CreatedSince":"1 day ago"}`,
    ].join("\n");
    const rows = parseImagesJson(stdout);
    assert.equal(rows[0]!.dangling, false);
    assert.equal(rows[1]!.dangling, true);
  });
});

describe("parseComposePs", () => {
  it("accepts a JSON array with Publishers", () => {
    const stdout = JSON.stringify([
      {
        Name: "app-web-1",
        Service: "web",
        State: "running",
        Status: "Up 1 minute",
        Publishers: [{ URL: "0.0.0.0", PublishedPort: 8080, TargetPort: 80, Protocol: "tcp" }],
      },
    ]);
    const rows = parseComposePs(stdout);
    assert.equal(rows.length, 1);
    assert.equal(rows[0]!.service, "web");
    assert.equal(rows[0]!.ports, "0.0.0.0:8080->80/tcp");
  });

  it("accepts NDJSON with a Ports string", () => {
    const rows = parseComposePs(
      `{"Name":"app-db-1","Service":"db","State":"exited","Status":"Exited (0)","Ports":""}\n`
    );
    assert.equal(rows[0]!.state, "exited");
  });
});

describe("parseJsonLines", () => {
  it("returns empty for blank / invalid", () => {
    assert.deepEqual(parseJsonLines(""), []);
    assert.deepEqual(parseJsonLines("not json"), []);
  });
});

describe("parseComposeFind", () => {
  it("keeps safe relative paths, drops traversal", () => {
    const files = parseComposeFind(
      ["/home/u/app/compose.yaml", "/home/u/app/deploy/docker-compose.yml", "/etc/passwd"].join("\n"),
      (line) => {
        if (line.startsWith("/home/u/app/")) return line.slice("/home/u/app/".length);
        return null;
      }
    );
    assert.deepEqual(
      files.map((f) => f.path),
      ["compose.yaml", "deploy/docker-compose.yml"]
    );
  });
});

describe("findComposeFilesCommand", () => {
  it("posix quotes the directory and prunes node_modules", () => {
    const cmd = findComposeFilesCommand("posix", "/home/u/re po");
    assert.match(cmd, /d='\/home\/u\/re po'/);
    assert.match(cmd, /find "\$d"/);
    assert.match(cmd, /-name 'node_modules'/);
    assert.match(cmd, /-name 'compose.yaml'/);
    assert.ok(COMPOSE_SKIP_DIRS.includes("node_modules"));
  });

  it("windows goes through EncodedCommand", () => {
    const cmd = findComposeFilesCommand("windows", "C:\\code\\repo");
    assert.match(cmd, /-EncodedCommand /);
    const script = Buffer.from(cmd.split(" ").pop()!, "base64").toString("utf16le");
    assert.match(script, /Get-ChildItem -LiteralPath \$p/);
    assert.match(script, /compose.yaml/);
  });
});

describe("classifyDockerFailure", () => {
  it("permission beats daemon when both words appear", () => {
    assert.equal(
      classifyDockerFailure(
        "permission denied while trying to connect to the Docker daemon socket at unix:///var/run/docker.sock",
        "",
        1
      ),
      "docker-permission"
    );
  });

  it("maps missing / daemon / compose / generic", () => {
    assert.equal(classifyDockerFailure("docker: command not found", "", 127), "docker-missing");
    assert.equal(
      classifyDockerFailure("Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?", "", 1),
      "docker-daemon"
    );
    assert.equal(
      classifyDockerFailure("docker: 'compose' is not a docker command.", "", 1),
      "compose-missing"
    );
    assert.equal(classifyDockerFailure("Error: No such container: web", "", 1), "command-failed");
  });
});

describe("parseDockerOpInput", () => {
  it("accepts the whitelist and rejects the rest", () => {
    assert.deepEqual(parseDockerOpInput({ op: "start", ref: "web" }), { op: "start", ref: "web" });
    assert.deepEqual(parseDockerOpInput({ op: "remove", ref: "abc123", force: true }), {
      op: "remove",
      ref: "abc123",
      force: true,
    });
    assert.deepEqual(parseDockerOpInput({ op: "image-prune" }), { op: "image-prune" });
    assert.deepEqual(parseDockerOpInput({ op: "compose-up", file: "compose.yaml" }), {
      op: "compose-up",
      file: "compose.yaml",
    });
    assert.equal("error" in parseDockerOpInput({ op: "start", ref: "-f" }), true);
    assert.equal("error" in parseDockerOpInput({ op: "exec", ref: "web" }), true);
    assert.equal("error" in parseDockerOpInput({ op: "compose-up", file: "../compose.yaml" }), true);
  });
});

describe("tail / logs cap / state helpers", () => {
  it("clamps tail and truncates oversized logs from the end", () => {
    assert.equal(clampTail(undefined), LOGS_DEFAULT_TAIL);
    assert.equal(clampTail(0), 1);
    assert.equal(clampTail(99999), 2000);
    const big = "x".repeat(LOGS_MAX_BYTES + 10);
    const { text, truncated } = truncateLogs(big);
    assert.equal(truncated, true);
    assert.equal(text.length, LOGS_MAX_BYTES);
    assert.equal(text.endsWith("x"), true);
  });

  it("infers running from Status when State is empty", () => {
    assert.equal(inferContainerState("", "Up 3 minutes"), "running");
    assert.equal(inferContainerState("Paused", "Up 3 minutes (Paused)"), "paused");
    assert.equal(isContainerRunning("running"), true);
    assert.equal(isContainerRunning("exited"), false);
  });
});

describe("errorDetail", () => {
  it("prefers the first non-empty stderr line", () => {
    assert.equal(errorDetail("\nError: boom\n", "ignored", 1), "Error: boom");
    assert.equal(errorDetail("", "", 3), "退出码 3");
  });
});
