package io.github.hythe55.conductor

import android.content.Intent
import android.os.Bundle
import android.webkit.WebView
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  private val files by lazy {
    AndroidFiles(applicationContext) {
      runOnUiThread {
        if (!isDestroyed) webView?.evaluateJavascript("window.dispatchEvent(new Event('conductorshare'))", null)
      }
    }
  }
  private val localNetwork by lazy { LocalNetwork(this) }
  private var webView: WebView? = null

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    // Opening the app from Recents brings back the share it was opened with, which was dealt with then.
    val fromRecents = (intent.flags and Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY) != 0
    if (savedInstanceState == null && !fromRecents) files.take(intent)
  }

  // Before the page loads, so it can use the files from the start.
  override fun onWebViewCreate(webView: WebView) {
    this.webView = webView
    webView.addJavascriptInterface(files, "ConductorAndroid")
    webView.addJavascriptInterface(localNetwork, "ConductorNetwork")
  }

  override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
    super.onRequestPermissionsResult(requestCode, permissions, grantResults)
    if (localNetwork.onResult(requestCode)) {
      webView?.evaluateJavascript("window.dispatchEvent(new Event('conductorlocalnetwork'))", null)
    }
  }

  // Files shared while the app is already open.
  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    setIntent(intent)
    files.take(intent)
  }
}
