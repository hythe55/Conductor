package io.github.hythe55.conductor

import android.app.Activity
import android.content.Intent
import android.webkit.JavascriptInterface
import androidx.core.content.FileProvider
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import java.util.concurrent.atomic.AtomicBoolean
import org.json.JSONObject

/**
 * Updates the app from its GitHub releases. The page finds a newer version;
 * this downloads its APK and hands it to Android's installer, which asks the
 * person to confirm. Android only installs it over this app if it's signed
 * with the same key. Exposed to the page as window.ConductorUpdate, which
 * hears how it went through [tell] as JSON.
 */
class AppUpdate(private val activity: Activity, private val tell: (String) -> Unit) {
  private val busy = AtomicBoolean(false)

  /** The version installed, like "0.1.13". */
  @JavascriptInterface
  fun version(): String = activity.packageManager.getPackageInfo(activity.packageName, 0).versionName ?: ""

  /** Downloads [version] and opens Android's installer with it. */
  @JavascriptInterface
  fun install(version: String) {
    // The page waits to hear back, so it hears even when nothing starts.
    if (!VERSION.matches(version)) return failed(Exception("$version isn't a version."))
    if (!busy.compareAndSet(false, true)) return failed(Exception("An update is already downloading."))
    Thread {
      try {
        val apk = download(version)
        activity.runOnUiThread {
          try {
            open(apk)
            tell("""{"state":"installing"}""")
          } catch (e: Exception) {
            failed(e)
          }
        }
      } catch (e: Exception) {
        failed(e)
      } finally {
        busy.set(false)
      }
    }.start()
  }

  private fun failed(e: Exception) {
    tell(JSONObject().put("state", "failed").put("message", e.message ?: e.toString()).toString())
  }

  private fun download(version: String): File {
    val url = URL("https://github.com/hythe55/Conductor/releases/download/v$version/Conductor_${version}_android-arm64.apk")
    val dir = File(activity.cacheDir, "updates")
    // Older downloads, leaving any the installer may still be reading.
    val hourAgo = System.currentTimeMillis() - 3_600_000
    dir.listFiles()?.filter { it.lastModified() < hourAgo }?.forEach { it.delete() }
    dir.mkdirs()
    val apk = File(dir, "Conductor-$version-${System.currentTimeMillis()}.apk")
    val connection = url.openConnection() as HttpURLConnection
    try {
      connection.connectTimeout = 15_000
      connection.readTimeout = 30_000
      if (connection.responseCode != 200) throw Exception("GitHub answered ${connection.responseCode}.")
      connection.inputStream.use { input -> apk.outputStream().use { input.copyTo(it) } }
    } finally {
      connection.disconnect()
    }
    return apk
  }

  private fun open(apk: File) {
    val uri = FileProvider.getUriForFile(activity, "${activity.packageName}.fileprovider", apk)
    val intent = Intent(Intent.ACTION_VIEW)
      .setDataAndType(uri, "application/vnd.android.package-archive")
      .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
    activity.startActivity(intent)
  }

  companion object {
    private val VERSION = Regex("""\d+\.\d+\.\d+""")
  }
}
