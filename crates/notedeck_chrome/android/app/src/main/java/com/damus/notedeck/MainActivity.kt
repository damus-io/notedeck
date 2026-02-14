package com.damus.notedeck

import android.content.Intent
import android.database.Cursor
import android.net.Uri
import android.os.Bundle
import android.provider.OpenableColumns
import android.util.Log
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup
import androidx.core.graphics.Insets
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import com.google.androidgamesdk.GameActivity
import java.io.ByteArrayOutputStream
import java.io.IOException

/**
 * Main activity for Notedeck Android, extending GameActivity for NDK/OpenGL rendering.
 *
 * Hosts the native Rust application via JNI and handles Android-specific concerns
 * like file picking, window insets, and touch event offset correction.
 */
class MainActivity : GameActivity() {

    companion object {
        const val REQUEST_CODE_PICK_FILE = 420
        private const val TAG = "MainActivity"
    }

    // Native method declarations (implemented in Rust via JNI)
    private external fun nativeOnFilePickedFailed(uri: String, e: String)
    private external fun nativeOnFilePickedWithContent(uriInfo: Array<Any?>, content: ByteArray)

    /**
     * Launch the system document picker for selecting one or more files.
     * Results are delivered to [onActivityResult].
     */
    fun openFilePicker() {
        val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
            type = "*/*"
            putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
            addCategory(Intent.CATEGORY_OPENABLE)
        }
        startActivityForResult(intent, REQUEST_CODE_PICK_FILE)
    }

    /**
     * Configure window insets so the content view respects system bars
     * (status bar, navigation bar) via margin adjustments.
     *
     * Also disables decor-fits-system-windows so the NDK side receives
     * correct inset values for keyboard visibility.
     */
    private fun setupInsets() {
        val content = getContent()
        ViewCompat.setOnApplyWindowInsetsListener(content) { v, windowInsets ->
            val insets: Insets = windowInsets.getInsets(WindowInsetsCompat.Type.systemBars())
            val mlp = v.layoutParams as ViewGroup.MarginLayoutParams
            mlp.topMargin = insets.top
            mlp.leftMargin = insets.left
            mlp.bottomMargin = insets.bottom
            mlp.rightMargin = insets.right
            v.layoutParams = mlp
            windowInsets
        }
        WindowCompat.setDecorFitsSystemWindows(window, false)
    }

    /**
     * Read and forward a picked file's metadata and content to Rust via JNI.
     * On failure, reports the error to Rust via [nativeOnFilePickedFailed].
     */
    private fun processSelectedFile(uri: Uri) {
        try {
            val content = readUriContent(uri)
            if (content == null) {
                Log.e(TAG, "Failed to read file content: $uri")
                nativeOnFilePickedFailed(uri.toString(), "Failed to read file content")
                return
            }
            nativeOnFilePickedWithContent(getUriInfo(uri), content)
        } catch (e: Exception) {
            Log.e(TAG, "Error processing file: $uri", e)
            nativeOnFilePickedFailed(uri.toString(), e.toString())
        }
    }

    /**
     * Query content resolver for file metadata: display name, size, and MIME type.
     *
     * @return Array of [displayName: String, size: Long, mimeType: String?],
     *         or empty array if the cursor yields no rows.
     * @throws Exception if the URI scheme is not "content://"
     */
    @Throws(Exception::class)
    private fun getUriInfo(uri: Uri): Array<Any?> {
        if (uri.scheme != "content") {
            throw Exception("uri should start with content://")
        }

        val cursor: Cursor? = contentResolver.query(uri, null, null, null, null)
        cursor?.use {
            while (it.moveToNext()) {
                val info = arrayOfNulls<Any>(3)
                var colIdx = it.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                info[0] = if (colIdx >= 0) it.getString(colIdx) else null
                colIdx = it.getColumnIndex(OpenableColumns.SIZE)
                info[1] = if (colIdx >= 0) it.getLong(colIdx) else 0L
                colIdx = it.getColumnIndex("mime_type")
                info[2] = if (colIdx >= 0) it.getString(colIdx) else null
                return info
            }
        }
        return emptyArray()
    }

    /**
     * Read the full byte content of a content:// URI.
     *
     * @return The file contents as a byte array, or null on I/O or security error.
     */
    private fun readUriContent(uri: Uri): ByteArray? {
        var inputStream: java.io.InputStream? = null
        var buffer: ByteArrayOutputStream? = null
        try {
            inputStream = contentResolver.openInputStream(uri)
            if (inputStream == null) {
                Log.e(TAG, "Could not open input stream for URI: $uri")
                return null
            }
            buffer = ByteArrayOutputStream()
            val data = ByteArray(8192)
            var bytesRead: Int
            while (inputStream.read(data).also { bytesRead = it } != -1) {
                buffer.write(data, 0, bytesRead)
            }
            val result = buffer.toByteArray()
            Log.d(TAG, "Successfully read ${result.size} bytes")
            return result
        } catch (e: IOException) {
            Log.e(TAG, "IOException while reading URI: $uri", e)
            return null
        } catch (e: SecurityException) {
            Log.e(TAG, "SecurityException while reading URI: $uri", e)
            return null
        } finally {
            try { inputStream?.close() } catch (e: IOException) {
                Log.e(TAG, "Error closing input stream", e)
            }
            try { buffer?.close() } catch (e: IOException) {
                Log.e(TAG, "Error closing buffer", e)
            }
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        setupInsets()
        super.onCreate(savedInstanceState)
    }

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)

        if (requestCode != REQUEST_CODE_PICK_FILE || resultCode != RESULT_OK) return
        if (data == null) return

        if (data.clipData != null) {
            val clipData = data.clipData!!
            for (i in 0 until clipData.itemCount) {
                processSelectedFile(clipData.getItemAt(i).uri)
            }
        } else if (data.data != null) {
            processSelectedFile(data.data!!)
        }
    }

    override fun onResume() {
        super.onResume()
    }

    override fun onPause() {
        super.onPause()
    }

    override fun onDestroy() {
        super.onDestroy()
    }

    /**
     * Offset touch events by the content view's screen position so coordinates
     * align with the GL surface when system-bar margins are applied.
     */
    override fun onTouchEvent(event: MotionEvent): Boolean {
        val location = IntArray(2)
        findViewById<View>(android.R.id.content).getLocationOnScreen(location)
        event.offsetLocation(-location[0].toFloat(), -location[1].toFloat())
        return super.onTouchEvent(event)
    }

    /** Configure immersive fullscreen with transient system bars. */
    private fun setupFullscreen() {
        WindowCompat.setDecorFitsSystemWindows(window, false)
        val controller = WindowCompat.getInsetsController(window, window.decorView)
        controller.systemBarsBehavior =
            WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        controller.hide(WindowInsetsCompat.Type.systemBars())
    }

    /** Request focus on the given view. */
    private fun focus(content: View) {
        content.isFocusable = true
        content.isFocusableInTouchMode = true
        content.requestFocus()
    }

    /** Get the root content view. */
    private fun getContent(): View {
        return window.decorView.findViewById(android.R.id.content)
    }
}
