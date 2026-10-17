package io.github.hythe55.conductor

import android.content.ContentResolver
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import android.provider.OpenableColumns
import android.util.Base64
import android.util.Log
import android.webkit.JavascriptInterface
import android.webkit.MimeTypeMap
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.io.FileOutputStream
import java.io.InputStream
import java.io.OutputStream
import kotlin.concurrent.thread

private const val TAG = "Conductor"

/**
 * Lets the app save files to Downloads, and read files shared to Conductor
 * from other apps so it can send them to a PC. The page reaches it as
 * `window.ConductorAndroid` (see src/lib/android.ts), and only passes strings
 * and numbers. The page's calls arrive on a background thread. `onShared` is
 * called, on another one, once the files from a share are ready.
 */
class AndroidFiles(private val context: Context, private val onShared: () -> Unit) {
  private class Shared(val uri: Uri, val name: String, val size: Long)
  private class Reading(val stream: InputStream, var position: Long)
  private class Saving(val out: OutputStream, val uri: Uri?, val file: File?, val name: String)

  private val resolver: ContentResolver get() = context.contentResolver
  private val lock = Any()

  // Shared files that can be read, by ID, and the latest share's until a PC is chosen for them.
  private val files = HashMap<Int, Shared>()
  private val reading = HashMap<Int, Reading>()
  private var waiting: List<Int> = emptyList()
  private var waitingText = false
  private var unreadable = 0
  private var nextFile = 1
  private var shares = 0

  private val saving = HashMap<Int, Saving>()
  private var nextSave = 1

  /** Keeps the files from a share, in place of any still waiting. */
  fun take(intent: Intent?) {
    if (intent == null) return
    val uris = when (intent.action) {
      Intent.ACTION_SEND -> listOfNotNull(stream(intent))
      Intent.ACTION_SEND_MULTIPLE -> streams(intent)
      else -> return
    }.ifEmpty { clipUris(intent) }
    val text = uris.isEmpty() && intent.hasExtra(Intent.EXTRA_TEXT)
    val share = synchronized(lock) { ++shares }
    // Other apps can be slow to describe their files, so this stays off the main thread.
    thread(name = "shared files") {
      val found = uris.mapNotNull { uri ->
        try {
          describe(uri)
        } catch (e: Exception) {
          Log.w(TAG, "reading a shared file: $e")
          null
        }
      }
      synchronized(lock) {
        // Shared again meanwhile.
        if (share != shares) return@thread
        waiting.forEach { forget(it) }
        waiting = found.map { f -> nextFile++.also { files[it] = f } }
        waitingText = text
        unreadable = uris.size - found.size
      }
      onShared()
    }
  }

  @Suppress("DEPRECATION")
  private fun stream(intent: Intent): Uri? =
    if (Build.VERSION.SDK_INT >= 33) intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri::class.java)
    else intent.getParcelableExtra(Intent.EXTRA_STREAM)

  @Suppress("DEPRECATION")
  private fun streams(intent: Intent): List<Uri> =
    (if (Build.VERSION.SDK_INT >= 33) intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM, Uri::class.java)
    else intent.getParcelableArrayListExtra<Uri>(Intent.EXTRA_STREAM)) ?: emptyList()

  private fun clipUris(intent: Intent): List<Uri> {
    val clip = intent.clipData ?: return emptyList()
    return (0 until clip.itemCount).mapNotNull { clip.getItemAt(it).uri }
  }

  private fun describe(uri: Uri): Shared {
    var name: String? = null
    var size = -1L
    resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE), null, null, null)?.use { c ->
      if (c.moveToFirst()) {
        val n = c.getColumnIndex(OpenableColumns.DISPLAY_NAME)
        val s = c.getColumnIndex(OpenableColumns.SIZE)
        if (n >= 0 && !c.isNull(n)) name = c.getString(n)
        if (s >= 0 && !c.isNull(s)) size = c.getLong(s)
      }
    }
    // Some apps don't say how big a file is, so count it.
    if (size < 0) size = count(uri)
    return Shared(uri, name ?: uri.lastPathSegment ?: "file", size)
  }

  /**
   * The files waiting to be sent, as {"files": [{id, name, size}], "text",
   * "unreadable"}: whether only text was shared, and how many files couldn't
   * be read.
   */
  @JavascriptInterface
  fun shared(): String = synchronized(lock) {
    val list = JSONArray()
    for (id in waiting) {
      val f = files[id] ?: continue
      list.put(JSONObject().put("id", id).put("name", f.name).put("size", f.size))
    }
    JSONObject().put("files", list).put("text", waitingText).put("unreadable", unreadable).toString()
  }

  private fun count(uri: Uri): Long {
    val stream = resolver.openInputStream(uri) ?: throw IllegalStateException("couldn't open $uri")
    stream.use {
      val buf = ByteArray(64 * 1024)
      var total = 0L
      while (true) {
        val n = it.read(buf)
        if (n < 0) return total
        total += n
      }
    }
  }

  /** The waiting files are being sent, so they stop waiting but can still be read. */
  @JavascriptInterface
  fun taken() {
    synchronized(lock) { stopWaiting() }
  }

  /** The waiting files won't be sent. */
  @JavascriptInterface
  fun clearShared() {
    synchronized(lock) {
      waiting.forEach { forget(it) }
      stopWaiting()
    }
  }

  private fun stopWaiting() {
    waiting = emptyList()
    waitingText = false
    unreadable = 0
  }

  /** Done with shared file `id`. */
  @JavascriptInterface
  fun close(id: Int) {
    synchronized(lock) { forget(id) }
  }

  private fun forget(id: Int) {
    files.remove(id)
    reading.remove(id)?.let { runCatching { it.stream.close() } }
  }

  /** Up to `length` bytes of shared file `id` from `offset`, in Base64. Null when it can't be read. */
  @JavascriptInterface
  fun read(id: Int, offset: Double, length: Int): String? = synchronized(lock) {
    try {
      val file = files[id] ?: return null
      val from = offset.toLong()
      var r = reading[id]
      if (r == null || r.position != from) {
        r?.let { runCatching { it.stream.close() } }
        reading.remove(id)
        val stream = resolver.openInputStream(file.uri) ?: return null
        var skipped = 0L
        val scratch = ByteArray(64 * 1024)
        while (skipped < from) {
          val n = stream.skip(from - skipped)
          if (n > 0) {
            skipped += n
            continue
          }
          // Some streams can't skip, so read past instead.
          val got = stream.read(scratch, 0, minOf(scratch.size.toLong(), from - skipped).toInt())
          if (got < 0) break
          skipped += got
        }
        if (skipped != from) {
          stream.close()
          return null
        }
        r = Reading(stream, skipped)
        reading[id] = r
      }
      val buf = ByteArray(length.coerceIn(0, 4 * 1024 * 1024))
      var n = 0
      while (n < buf.size) {
        val got = r.stream.read(buf, n, buf.size - n)
        if (got < 0) break
        n += got
      }
      r.position += n
      Base64.encodeToString(buf, 0, n, Base64.NO_WRAP)
    } catch (e: Exception) {
      Log.w(TAG, "reading a shared file: $e")
      null
    }
  }

  /** Whether saved files go in the Downloads folder everyone sees, rather than Conductor's own. */
  @JavascriptInterface
  fun savesToDownloads(): Boolean = Build.VERSION.SDK_INT >= 29

  /** Starts saving a file called `name`. Returns an ID for the calls below, or -1. */
  @JavascriptInterface
  fun beginSave(name: String): Int = synchronized(lock) {
    try {
      val clean = name.replace(Regex("[\\\\/:*?\"<>|\\x00-\\x1f]"), "_").trim().ifEmpty { "file" }
      val id = nextSave++
      if (Build.VERSION.SDK_INT >= 29) {
        val values = ContentValues().apply {
          put(MediaStore.MediaColumns.DISPLAY_NAME, clean)
          mimeType(clean)?.let { put(MediaStore.MediaColumns.MIME_TYPE, it) }
          put(MediaStore.MediaColumns.RELATIVE_PATH, Environment.DIRECTORY_DOWNLOADS)
          put(MediaStore.MediaColumns.IS_PENDING, 1)
        }
        val uri = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values) ?: return -1
        val out = try {
          resolver.openOutputStream(uri)
        } catch (e: Exception) {
          Log.w(TAG, "saving a file: $e")
          null
        }
        if (out == null) {
          runCatching { resolver.delete(uri, null, null) }
          return -1
        }
        saving[id] = Saving(out, uri, null, clean)
      } else {
        // Older Android needs a permission for the shared Downloads folder, so these stay in the app's own.
        val dir = context.getExternalFilesDir(Environment.DIRECTORY_DOWNLOADS) ?: return -1
        dir.mkdirs()
        val file = unique(dir, clean)
        saving[id] = Saving(FileOutputStream(file), null, file, file.name)
      }
      id
    } catch (e: Exception) {
      Log.w(TAG, "saving a file: $e")
      -1
    }
  }

  private fun mimeType(name: String): String? =
    name.substringAfterLast('.', "").lowercase().takeIf { it.isNotEmpty() }
      ?.let { MimeTypeMap.getSingleton().getMimeTypeFromExtension(it) }

  private fun unique(dir: File, name: String): File {
    val stem = name.substringBeforeLast('.')
    val ext = name.substring(stem.length)
    var file = File(dir, name)
    var n = 1
    while (file.exists()) file = File(dir, "$stem (${n++})$ext")
    return file
  }

  /** Adds Base64 `data` to save `id`. Returns whether it was written. */
  @JavascriptInterface
  fun writeSave(id: Int, data: String): Boolean = synchronized(lock) {
    try {
      val s = saving[id] ?: return false
      s.out.write(Base64.decode(data, Base64.DEFAULT))
      true
    } catch (e: Exception) {
      Log.w(TAG, "saving a file: $e")
      false
    }
  }

  /** Finishes save `id`. Returns the name it was saved under, or an empty string. */
  @JavascriptInterface
  fun finishSave(id: Int): String = synchronized(lock) {
    val s = saving.remove(id) ?: return ""
    try {
      s.out.close()
      if (s.uri == null) return s.name
      resolver.update(s.uri, ContentValues().apply { put(MediaStore.MediaColumns.IS_PENDING, 0) }, null, null)
      // Android renames a file whose name is taken.
      resolver.query(s.uri, arrayOf(MediaStore.MediaColumns.DISPLAY_NAME), null, null, null)?.use { c ->
        if (c.moveToFirst() && !c.isNull(0)) return c.getString(0)
      }
      s.name
    } catch (e: Exception) {
      Log.w(TAG, "saving a file: $e")
      discard(s)
      ""
    }
  }

  /** Throws away save `id`. */
  @JavascriptInterface
  fun abortSave(id: Int) {
    synchronized(lock) { saving.remove(id)?.let { discard(it) } }
  }

  private fun discard(s: Saving) {
    runCatching { s.out.close() }
    runCatching { s.uri?.let { resolver.delete(it, null, null) } }
    runCatching { s.file?.delete() }
  }
}
