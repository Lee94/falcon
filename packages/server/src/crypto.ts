import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

/**
 * 凭据静态加密：AES-256-GCM，密钥为后端数据目录下的 secret.key。
 * 保护级别：防数据库文件单独泄露，不防整机被攻破。
 */
export class SecretBox {
  private key: Buffer;

  constructor(dataDir: string) {
    const keyPath = path.join(dataDir, "secret.key");
    if (!fs.existsSync(keyPath)) {
      fs.writeFileSync(keyPath, crypto.randomBytes(32), { mode: 0o600 });
    }
    this.key = fs.readFileSync(keyPath);
    if (this.key.length !== 32) {
      throw new Error(`secret.key 已损坏（长度 ${this.key.length}，应为 32 字节）`);
    }
  }

  encrypt(plaintext: string): string {
    const iv = crypto.randomBytes(12);
    const cipher = crypto.createCipheriv("aes-256-gcm", this.key, iv);
    const enc = Buffer.concat([cipher.update(plaintext, "utf8"), cipher.final()]);
    const tag = cipher.getAuthTag();
    return Buffer.concat([iv, tag, enc]).toString("base64");
  }

  decrypt(payload: string): string {
    const buf = Buffer.from(payload, "base64");
    const iv = buf.subarray(0, 12);
    const tag = buf.subarray(12, 28);
    const enc = buf.subarray(28);
    const decipher = crypto.createDecipheriv("aes-256-gcm", this.key, iv);
    decipher.setAuthTag(tag);
    return Buffer.concat([decipher.update(enc), decipher.final()]).toString("utf8");
  }
}

// ---- 访问密码哈希（scrypt） ----

export function hashPassword(password: string): string {
  const salt = crypto.randomBytes(16);
  const hash = crypto.scryptSync(password, salt, 32);
  return `${salt.toString("hex")}:${hash.toString("hex")}`;
}

export function verifyPassword(password: string, stored: string): boolean {
  const [saltHex, hashHex] = stored.split(":");
  if (!saltHex || !hashHex) return false;
  const hash = crypto.scryptSync(password, Buffer.from(saltHex, "hex"), 32);
  return crypto.timingSafeEqual(hash, Buffer.from(hashHex, "hex"));
}
