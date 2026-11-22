package com.falcon.android.net

import com.falcon.android.model.TailnetProxy
import okhttp3.Dns
import java.io.ByteArrayOutputStream
import java.io.EOFException
import java.io.IOException
import java.io.InputStream
import java.net.Inet6Address
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.Socket
import java.net.SocketAddress
import javax.net.SocketFactory

/**
 * 不做系统 DNS 解析，把原始主机名挂在占位 InetAddress 上透传给 [TailnetSocketFactory]，
 * 由 tsnet 的 SOCKS5 代理完成 MagicDNS 解析（ADR 0030 决定二）。
 */
object TailnetDns : Dns {
    override fun lookup(hostname: String): List<InetAddress> {
        val ipv4 = parseIpv4Bytes(hostname)
        if (ipv4 != null) {
            return listOf(InetAddress.getByAddress(hostname, ipv4))
        }
        return listOf(InetAddress.getByAddress(hostname, byteArrayOf(127, 0, 0, 1)))
    }

    private fun parseIpv4Bytes(host: String): ByteArray? {
        val parts = host.split('.')
        if (parts.size != 4) return null
        val out = ByteArray(4)
        for (i in 0..3) {
            val n = parts[i].toIntOrNull() ?: return null
            if (n !in 0..255) return null
            out[i] = n.toByte()
        }
        return out
    }
}

/**
 * 经内置 Tailscale 回环 SOCKS5 代理建连的 OkHttp SocketFactory（RFC 1928 + RFC 1929）。
 */
class TailnetSocketFactory(private val proxy: TailnetProxy) : SocketFactory() {

    override fun createSocket(): Socket = Socks5Socket(proxy)

    override fun createSocket(host: String, port: Int): Socket =
        Socks5Socket(proxy).apply { connect(InetSocketAddress.createUnresolved(host, port)) }

    override fun createSocket(host: String, port: Int, localHost: InetAddress, localPort: Int): Socket =
        Socks5Socket(proxy).apply {
            bind(InetSocketAddress(localHost, localPort))
            connect(InetSocketAddress.createUnresolved(host, port))
        }

    override fun createSocket(host: InetAddress, port: Int): Socket =
        Socks5Socket(proxy).apply { connect(InetSocketAddress(host, port)) }

    override fun createSocket(
        address: InetAddress,
        port: Int,
        localAddress: InetAddress,
        localPort: Int
    ): Socket = Socks5Socket(proxy).apply {
        bind(InetSocketAddress(localAddress, localPort))
        connect(InetSocketAddress(address, port))
    }

    private class Socks5Socket(private val proxy: TailnetProxy) : Socket() {
        override fun connect(endpoint: SocketAddress) {
            connect(endpoint, 0)
        }

        override fun connect(endpoint: SocketAddress, timeout: Int) {
            val target = endpoint as? InetSocketAddress
                ?: throw IOException("不支持的目标地址类型：$endpoint")
            val targetHost = target.hostString
                ?: throw IOException("目标主机名为空")
            val targetPort = target.port
            val proxyAddr = InetSocketAddress(proxy.host, proxy.port)

            super.connect(proxyAddr, timeout)
            val prevSoTimeout = soTimeout
            try {
                if (timeout > 0) {
                    soTimeout = timeout
                }
                performHandshake(targetHost, targetPort)
            } catch (e: IOException) {
                runCatching { close() }
                throw e
            } finally {
                runCatching { soTimeout = prevSoTimeout }
            }
        }

        private fun performHandshake(targetHost: String, targetPort: Int) {
            val out = getOutputStream()
            val inp = getInputStream()

            val hasAuth = proxy.user.isNotEmpty() || proxy.pass.isNotEmpty()
            if (hasAuth) {
                out.write(byteArrayOf(0x05, 0x02, 0x02, 0x00))
            } else {
                out.write(byteArrayOf(0x05, 0x01, 0x00))
            }
            out.flush()

            val greeting = readExact(inp, 2)
            if (greeting[0].toInt() != 0x05) {
                throw IOException("SOCKS5 代理版本异常：${greeting[0]}")
            }
            when (greeting[1].toInt() and 0xFF) {
                0x00 -> Unit
                0x02 -> {
                    val userBytes = proxy.user.toByteArray(Charsets.UTF_8)
                    val passBytes = proxy.pass.toByteArray(Charsets.UTF_8)
                    if (userBytes.size > 255 || passBytes.size > 255) {
                        throw IOException("SOCKS5 口令超长")
                    }
                    val auth = ByteArrayOutputStream(3 + userBytes.size + passBytes.size)
                    auth.write(0x01)
                    auth.write(userBytes.size)
                    auth.write(userBytes)
                    auth.write(passBytes.size)
                    auth.write(passBytes)
                    out.write(auth.toByteArray())
                    out.flush()

                    val authResp = readExact(inp, 2)
                    if (authResp[1].toInt() != 0x00) {
                        throw IOException("SOCKS5 代理认证被拒绝")
                    }
                }
                else -> throw IOException("SOCKS5 代理不支持可用的认证方式")
            }

            val cleanedHost = targetHost.trim().removePrefix("[").removeSuffix("]")
            val req = ByteArrayOutputStream(32)
            req.write(0x05) // VER
            req.write(0x01) // CONNECT
            req.write(0x00) // RSV
            val ipv4 = parseIpv4(cleanedHost)
            val ipv6 = if (ipv4 == null && cleanedHost.contains(':')) {
                runCatching { InetAddress.getByName(cleanedHost) as? Inet6Address }.getOrNull()
            } else null

            when {
                ipv4 != null -> {
                    req.write(0x01)
                    req.write(ipv4)
                }
                ipv6 != null -> {
                    req.write(0x04)
                    req.write(ipv6.address)
                }
                else -> {
                    val hostBytes = cleanedHost.toByteArray(Charsets.UTF_8)
                    if (hostBytes.isEmpty() || hostBytes.size > 255) {
                        throw IOException("目标主机名无效：$cleanedHost")
                    }
                    req.write(0x03)
                    req.write(hostBytes.size)
                    req.write(hostBytes)
                }
            }
            req.write((targetPort ushr 8) and 0xFF)
            req.write(targetPort and 0xFF)
            out.write(req.toByteArray())
            out.flush()

            val head = readExact(inp, 4)
            if (head[0].toInt() != 0x05) {
                throw IOException("SOCKS5 响应版本异常：${head[0]}")
            }
            val rep = head[1].toInt() and 0xFF
            if (rep != 0x00) {
                throw IOException("经内置 Tailscale 连接 $cleanedHost:$targetPort 失败（SOCKS5 错误码 $rep）")
            }
            val skipBytes = when (head[3].toInt() and 0xFF) {
                0x01 -> 4 + 2
                0x04 -> 16 + 2
                0x03 -> (readExact(inp, 1)[0].toInt() and 0xFF) + 2
                else -> throw IOException("SOCKS5 未知绑定地址类型")
            }
            readExact(inp, skipBytes)
        }

        private fun parseIpv4(host: String): ByteArray? {
            val parts = host.split('.')
            if (parts.size != 4) return null
            val out = ByteArray(4)
            for (i in 0..3) {
                val n = parts[i].toIntOrNull() ?: return null
                if (n !in 0..255) return null
                out[i] = n.toByte()
            }
            return out
        }

        private fun readExact(inp: InputStream, count: Int): ByteArray {
            val buf = ByteArray(count)
            var offset = 0
            while (offset < count) {
                val read = inp.read(buf, offset, count - offset)
                if (read < 0) throw EOFException("SOCKS5 代理提前断开连接")
                offset += read
            }
            return buf
        }
    }
}
