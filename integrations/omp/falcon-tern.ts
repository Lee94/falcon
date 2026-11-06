// Falcon Tern display bridge.
import { randomBytes, timingSafeEqual } from "node:crypto";
import {
	chmodSync,
	closeSync,
	constants,
	fstatSync,
	lstatSync,
	mkdirSync,
	openSync,
	readFileSync,
	renameSync,
	unlinkSync,
	writeFileSync,
} from "node:fs";
import type { Stats } from "node:fs";
import { createServer } from "node:net";
import type { Server, Socket } from "node:net";
import { homedir } from "node:os";
import { join } from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";

type DisplayTui = Parameters<Parameters<ExtensionContext["ui"]["custom"]>[0]>[0];
type BridgeState = { dispose(): void };
type Metadata = {
	version: 1;
	pid: number;
	sessionId: string;
	sessiondId: string;
	endpoint: string;
	token: string;
};

const bridgeKey = Symbol.for("falcon.tern.display-bridge.v1");
const globals = globalThis as typeof globalThis & { [bridgeKey]?: BridgeState };
const requestLimit = 4096;
const socketDeadlineMs = 2000;

function ownedPath(path: string, uid: number): Stats | undefined {
	try {
		const stat = lstatSync(path);
		if (stat.isSymbolicLink() || stat.uid !== uid) {
			throw new Error(`Unsafe Falcon display bridge path: ${path}`);
		}
		return stat;
	} catch (error) {
		if ((error as NodeJS.ErrnoException)?.code === "ENOENT") return undefined;
		throw error;
	}
}

function directory(path: string, uid: number, create: boolean, privateMode = false): void {
	let stat = ownedPath(path, uid);
	if (!stat && create) {
		mkdirSync(path, { mode: 0o700 });
		stat = ownedPath(path, uid);
	}
	if (
		!stat?.isDirectory() ||
		(stat.mode & 0o022) !== 0 ||
		(privateMode && (stat.mode & 0o777) !== 0o700)
	) {
		throw new Error(`Unsafe Falcon display bridge directory: ${path}`);
	}
}

function privateFile(path: string, uid: number): Stats | undefined {
	const stat = ownedPath(path, uid);
	if (stat && (!stat.isFile() || (stat.mode & 0o777) !== 0o600 || stat.nlink !== 1)) {
		throw new Error(`Unsafe Falcon display bridge metadata: ${path}`);
	}
	return stat;
}

function sameFile(left: Stats, right: Stats): boolean {
	return left.dev === right.dev && left.ino === right.ino;
}

function removeOwnMetadata(path: string, metadata: Metadata, uid: number): void {
	const stat = privateFile(path, uid);
	if (!stat || stat.size > requestLimit) return;
	const fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
	try {
		if (!sameFile(stat, fstatSync(fd))) return;
		const current: unknown = JSON.parse(readFileSync(fd, "utf8"));
		if (
			current && typeof current === "object" &&
			"token" in current && current.token === metadata.token &&
			"endpoint" in current && current.endpoint === metadata.endpoint
		) {
			const latest = privateFile(path, uid);
			if (latest && sameFile(stat, latest)) unlinkSync(path);
		}
	} finally {
		closeSync(fd);
	}
}

function writeMetadata(path: string, temporary: string, metadata: Metadata, uid: number): void {
	privateFile(path, uid);
	const fd = openSync(
		temporary,
		constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW,
		0o600,
	);
	let created: Stats | undefined;
	let renamed = false;
	try {
		created = fstatSync(fd);
		if (created.uid !== uid || !created.isFile() || (created.mode & 0o777) !== 0o600 || created.nlink !== 1) {
			throw new Error("Unsafe Falcon display bridge metadata permissions");
		}
		writeFileSync(fd, `${JSON.stringify(metadata)}\n`, "utf8");
		privateFile(path, uid);
		renameSync(temporary, path);
		renamed = true;
		privateFile(path, uid);
	} finally {
		closeSync(fd);
		if (!renamed && created) {
			try {
				const latest = ownedPath(temporary, uid);
				if (latest?.isFile() && sameFile(created, latest)) unlinkSync(temporary);
			} catch { /* Only the file created by this publication belongs to it. */ }
		}
	}
}

export default function falconTern(pi: ExtensionAPI): void {
	const sessionId = process.env.FALCON_SESSION_ID;
	const sessiondId = process.env.FALCON_SESSIOND_ID;
	if (!sessionId || !sessiondId || typeof process.getuid !== "function") return;
	const uid = process.getuid();
	let current: BridgeState | undefined;

	pi.on("session_start", async (_event, ctx) => {
		// Factories also run in subagents, which must never retire the main TUI's bridge.
		if (ctx.mode !== "tui" || !ctx.hasUI || ctx.agent.kind !== "main") return;
		globals[bridgeKey]?.dispose();

		let closed = false;
		let server: Server | undefined;
		let tui: DisplayTui | undefined;
		let active: Socket | undefined;
		let socketStat: Stats | undefined;
		let published = false;
		let cancelListen: (() => void) | undefined;
		const sockets = new Set<Socket>();
		const abortCapture = new AbortController();
		const registry = join(homedir(), ".omp", "run", "falcon-tern");
		const generation = randomBytes(8).toString("hex");
		const metadataPath = join(registry, `${process.pid}.json`);
		const temporary = join(registry, `${process.pid}-${generation}.tmp`);
		const metadata: Metadata = {
			version: 1,
			pid: process.pid,
			sessionId,
			sessiondId,
			endpoint: join(registry, `${process.pid}-${generation}.sock`),
			token: randomBytes(32).toString("hex"),
		};
		const token = Buffer.from(metadata.token, "hex");
		const state: BridgeState = {
			dispose() {
				if (closed) return;
				closed = true;
				abortCapture.abort();
				cancelListen?.();
				for (const socket of sockets) socket.destroy();
				server?.close();
				// A replaced generation may finish closing after its successor has published.
				// Only its random endpoint and matching metadata token belong to this state.
				try {
					directory(registry, uid, false, true);
					if (published) removeOwnMetadata(metadataPath, metadata, uid);
				} catch { /* A changed or unsafe path is never removed. */ }
				try {
					const stat = ownedPath(metadata.endpoint, uid);
					if (socketStat && stat?.isSocket() && sameFile(socketStat, stat)) {
						unlinkSync(metadata.endpoint);
					}
				} catch { /* The server may already have unlinked its socket. */ }
				if (globals[bridgeKey] === state) delete globals[bridgeKey];
			},
		};
		current = state;
		globals[bridgeKey] = state;

		try {
			await ctx.ui.custom<void>((display, _theme, _keys, done) => {
				tui = display;
				// Close before the host installs the empty overlay; the editor and draft stay intact.
				queueMicrotask(() => done(undefined));
				return { render: () => [], invalidate() {} };
			}, { overlay: true, signal: abortCapture.signal });
			if (closed || !tui) return;

			const home = homedir();
			directory(home, uid, false);
			directory(join(home, ".omp"), uid, true);
			directory(join(home, ".omp", "run"), uid, true);
			directory(registry, uid, true, true);
			privateFile(metadataPath, uid);
			server = createServer(socket => {
				sockets.add(socket);
				let handled = false;
				const deadline = setTimeout(() => socket.destroy(), socketDeadlineMs);
				deadline.unref();
				socket.on("error", () => socket.destroy());
				socket.once("close", () => {
					clearTimeout(deadline);
					sockets.delete(socket);
					if (active === socket) active = undefined;
				});
				const reply = (ok: boolean, error?: string) => {
					if (handled) return;
					handled = true;
					socket.end(`${JSON.stringify(ok ? { v: 1, ok: true } : { v: 1, ok: false, error })}\n`);
				};
				if (closed || active) {
					reply(false, closed ? "Bridge closed" : "Bridge busy");
					return;
				}
				active = socket;
				const input = Buffer.allocUnsafe(requestLimit);
				let length = 0;
				socket.on("data", (chunk: Buffer) => {
					if (handled) return;
					if (length + chunk.length > requestLimit) {
						reply(false, "Request exceeds 4096 bytes");
						return;
					}
					chunk.copy(input, length);
					length += chunk.length;
					const newline = input.subarray(0, length).indexOf(0x0a);
					if (newline < 0) return;
					if (newline !== length - 1) {
						reply(false, "Only one request is permitted");
						return;
					}
					let request: unknown;
					try {
						request = JSON.parse(input.toString("utf8", 0, newline));
					} catch {
						reply(false, "Invalid JSON request");
						return;
					}
					if (
						!request || typeof request !== "object" ||
						!("v" in request) || request.v !== 1 ||
						!("op" in request) || request.op !== "reprobe" ||
						!("token" in request) || typeof request.token !== "string" ||
						!/^[0-9a-f]{64}$/.test(request.token) ||
						!timingSafeEqual(Buffer.from(request.token, "hex"), token)
					) {
						reply(false, "Invalid version, operation, or authentication");
						return;
					}
					try {
						// The public lifecycle renegotiates native/ANSI display without replacing components.
						tui!.stop();
						tui!.start();
						tui!.requestRender(true);
						reply(true);
					} catch {
						reply(false, "Display refresh failed");
					}
				});
			});
			server.maxConnections = 16;
			server.on("error", () => {
				if (published) state.dispose();
			});
			const listening = Promise.withResolvers<void>();
			cancelListen = listening.resolve;
			server.once("error", listening.reject);
			server.listen(metadata.endpoint, () => {
				server!.off("error", listening.reject);
				listening.resolve();
			});
			await listening.promise;
			cancelListen = undefined;
			if (closed) return;
			socketStat = ownedPath(metadata.endpoint, uid);
			if (!socketStat?.isSocket()) throw new Error("Unsafe Falcon display bridge socket");
			chmodSync(metadata.endpoint, 0o600);
			const secured = ownedPath(metadata.endpoint, uid);
			if (!secured || !sameFile(socketStat, secured) || (secured.mode & 0o777) !== 0o600) {
				throw new Error("Unsafe Falcon display bridge socket permissions");
			}
			// The private registry protects the socket until chmod completes and metadata is published.
			published = true;
			writeMetadata(metadataPath, temporary, metadata, uid);
			server.unref();
		} catch (error) {
			const cancelled = closed;
			state.dispose();
			if (!cancelled) throw error;
		}
	});

	pi.on("session_shutdown", () => {
		current?.dispose();
		current = undefined;
	});
}
