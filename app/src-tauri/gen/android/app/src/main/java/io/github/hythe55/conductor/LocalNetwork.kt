package io.github.hythe55.conductor

import android.app.Activity
import android.content.Context
import android.content.pm.PackageManager
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.os.Build
import android.webkit.JavascriptInterface
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.Inet4Address
import java.net.InetAddress
import org.json.JSONArray

/**
 * Finds PCs on the phone's local network and sends Wake-on-LAN packets over
 * Wi-Fi (or Ethernet). Tailscale cannot carry LAN broadcast packets.
 * Exposed to the page as window.ConductorNetwork.
 */
class LocalNetwork(private val activity: Activity) {
  private val connectivity by lazy {
    activity.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
  }

  /** Whether Android lets the app reach the local network now. */
  @JavascriptInterface
  fun allowed(): Boolean =
    Build.VERSION.SDK_INT < 37 || activity.checkSelfPermission(PERMISSION) == PackageManager.PERMISSION_GRANTED

  /** Asks the person for local network access, when Android requires it. */
  @JavascriptInterface
  fun ask() {
    activity.runOnUiThread { activity.requestPermissions(arrayOf(PERMISSION), REQUEST) }
  }

  /** True when the phone is on one of the target's LANs. No packets are sent. */
  @JavascriptInterface
  fun canWake(adapters: String): Boolean =
    runCatching { routes(adapters).isNotEmpty() }.getOrDefault(false)

  /**
   * Sends magic packets directly on the matching physical network.
   * This is called on Android's JavaScript bridge thread, not the UI thread.
   * Returns false if no matching network exists or all sends fail.
   */
  @JavascriptInterface
  fun wake(adapters: String): Boolean {
    if (!allowed()) return false
    return runCatching {
      var sent = false
      for (route in routes(adapters)) {
        try {
          DatagramSocket().use { socket ->
            // Route the broadcast over Wi-Fi/Ethernet rather than the VPN.
            route.network.bindSocket(socket)
            socket.broadcast = true
            val packet = ByteArray(102) { i ->
              if (i < 6) 0xff.toByte() else route.target.mac[(i - 6) % 6]
            }
            val destinations = listOf(-1, route.target.address or route.target.mask.inv(), route.target.address)
            for (round in 0 until 3) {
              if (round > 0) Thread.sleep(300)
              for (address in destinations) {
                val destination = InetAddress.getByAddress(bytes(address))
                for (port in listOf(9, 7)) {
                  try {
                    socket.send(DatagramPacket(packet, packet.size, destination, port))
                    sent = true
                  } catch (_: Exception) {
                    // Try the other broadcast address and port.
                  }
                }
              }
            }
          }
        } catch (_: Exception) {
          // One network may have gone away; try the other matching routes.
        }
      }
      sent
    }.getOrDefault(false)
  }

  /** True when [requestCode] was this permission's, so the page should be told. */
  fun onResult(requestCode: Int): Boolean = requestCode == REQUEST

  private data class Target(val mac: ByteArray, val address: Int, val mask: Int, val prefix: Int)
  private data class Route(val network: Network, val target: Target)

  private fun routes(json: String): List<Route> {
    val items = JSONArray(json)
    val targets = buildList {
      for (i in 0 until items.length()) {
        val item = items.optJSONObject(i) ?: continue
        val address = parseIp(item.optString("address")) ?: continue
        val mac = parseMac(item.optString("mac")) ?: continue
        val prefix = item.optInt("prefix", -1)
        if (prefix !in 1..32) continue
        val mask = -1 shl (32 - prefix)
        add(Target(mac, address, mask, prefix))
      }
    }
    if (targets.isEmpty()) return emptyList()

    val result = mutableListOf<Route>()
    for (network in connectivity.allNetworks) {
      val caps = connectivity.getNetworkCapabilities(network) ?: continue
      if (caps.hasTransport(NetworkCapabilities.TRANSPORT_VPN)) continue
      if (!caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) &&
          !caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET)) continue
      val properties = connectivity.getLinkProperties(network) ?: continue
      for (link in properties.linkAddresses) {
        val address = link.address as? Inet4Address ?: continue
        if (address.isLoopbackAddress || address.isLinkLocalAddress) continue
        val local = ipInt(address.address)
        for (target in targets) {
          // Match both the subnet and prefix, just as the Windows host does.
          if (link.prefixLength == target.prefix && (local and target.mask) == (target.address and target.mask)) {
            result.add(Route(network, target))
          }
        }
      }
    }
    return result
  }

  private fun parseIp(value: String): Int? {
    val parts = value.split('.')
    if (parts.size != 4) return null
    var address = 0
    for (part in parts) {
      if (part.isEmpty() || part.length > 3 || part.any { !it.isDigit() }) return null
      val octet = part.toIntOrNull() ?: return null
      if (octet !in 0..255) return null
      address = (address shl 8) or octet
    }
    return address
  }

  private fun parseMac(value: String): ByteArray? {
    val parts = value.replace('-', ':').split(':')
    if (parts.size != 6) return null
    val bytes = ByteArray(6)
    for ((i, part) in parts.withIndex()) {
      if (part.length != 2) return null
      val octet = part.toIntOrNull(16) ?: return null
      bytes[i] = octet.toByte()
    }
    return bytes
  }

  private fun ipInt(address: ByteArray): Int =
    address.fold(0) { value, octet -> (value shl 8) or (octet.toInt() and 0xff) }

  private fun bytes(address: Int): ByteArray =
    byteArrayOf((address ushr 24).toByte(), (address ushr 16).toByte(),
      (address ushr 8).toByte(), address.toByte())

  companion object {
    const val PERMISSION = "android.permission.ACCESS_LOCAL_NETWORK"
    private const val REQUEST = 4280
  }
}
