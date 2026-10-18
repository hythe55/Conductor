package io.github.hythe55.conductor

import android.app.Activity
import android.content.pm.PackageManager
import android.os.Build
import android.webkit.JavascriptInterface

/**
 * Lets the page ask for Android's local network permission, which it needs to
 * look for PCs on the same Wi-Fi. PCs themselves are reached over Tailscale,
 * which doesn't need it. Exposed to the page as window.ConductorNetwork.
 */
class LocalNetwork(private val activity: Activity) {
  /** Whether Android lets the app reach the local network now. */
  @JavascriptInterface
  fun allowed(): Boolean =
    Build.VERSION.SDK_INT < 37 || activity.checkSelfPermission(PERMISSION) == PackageManager.PERMISSION_GRANTED

  /**
   * Asks the person, unless they said no for good. The page hears the answer as
   * a "conductorlocalnetwork" event, once [onResult] is called with it.
   */
  @JavascriptInterface
  fun ask() {
    activity.runOnUiThread { activity.requestPermissions(arrayOf(PERMISSION), REQUEST) }
  }

  /** True when [requestCode] was this permission's, so the page should be told. */
  fun onResult(requestCode: Int): Boolean = requestCode == REQUEST

  companion object {
    const val PERMISSION = "android.permission.ACCESS_LOCAL_NETWORK"
    private const val REQUEST = 4280
  }
}
