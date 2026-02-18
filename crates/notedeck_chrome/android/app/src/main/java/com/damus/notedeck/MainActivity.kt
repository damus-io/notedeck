package com.damus.notedeck

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.database.Cursor
import android.net.Uri
import android.os.Bundle
import android.provider.OpenableColumns
import android.util.Log
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import androidx.core.graphics.Insets
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import com.damus.notedeck.service.NotepushClient
import com.damus.notedeck.service.NotificationsService
import com.google.androidgamesdk.GameActivity
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import java.io.ByteArrayOutputStream
import java.io.IOException

/**
 * Main activity for Notedeck Android, extending GameActivity for NDK/OpenGL rendering.
 *
 * Hosts the native Rust application via JNI and handles Android-specific concerns
 * like file picking, window insets, touch event offset correction, and notification
 * management (FCM registration, permission requests, mode persistence).
 */
class MainActivity : GameActivity() {

    companion object {
        const val REQUEST_CODE_PICK_FILE = 420
        const val REQUEST_CODE_NOTIFICATION_PERMISSION = 1001
        private const val PREFS_NAME = "notedeck_notifications"
        private const val KEY_MODE = "notification_mode"
        private const val TAG = "MainActivity"
    }

    private val notificationScope = CoroutineScope(Dispatchers.IO + SupervisorJob())
    private val notepushClient = NotepushClient()

    // Native method declarations (implemented in Rust via JNI)
    private external fun nativeOnFilePickedFailed(uri: String, e: String)
    private external fun nativeOnFilePickedWithContent(uriInfo: Array<Any?>, content: ByteArray)
    private external fun nativeOnNotificationPermissionResult(granted: Boolean)
    private external fun nativeOnDeepLink(eventId: String, eventKind: Int, authorPubkey: String)

    /**
     * Bridge notification-tap extras from Android intents into Rust deep-link state.
     */
    private fun handleDeepLinkIntent(intent: Intent?) {
        val eventId = intent?.getStringExtra("event_id") ?: return
        val eventKind = intent.getIntExtra("event_kind", 0)
        val authorPubkey = intent.getStringExtra("author_pubkey") ?: ""
        nativeOnDeepLink(eventId, eventKind, authorPubkey)
    }

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
            if (it.moveToFirst()) {
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
        handleDeepLinkIntent(intent)
    }

    override fun onNewIntent(intent: Intent?) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleDeepLinkIntent(intent)
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
        notificationScope.cancel()
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

    // =========================================================================
    // Notification methods (called from Rust via JNI)
    // =========================================================================

    /**
     * Read the persisted notification mode from SharedPreferences.
     *
     * @return Mode index: 0 = FCM, 1 = Native, 2 = Disabled (default).
     */
    fun getNotificationMode(): Int {
        return getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .getInt(KEY_MODE, 2)
    }

    /**
     * Persist the notification mode to SharedPreferences.
     *
     * @param mode 0 = FCM, 1 = Native, 2 = Disabled.
     */
    fun setNotificationMode(mode: Int) {
        getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit()
            .putInt(KEY_MODE, mode)
            .apply()
    }

    /**
     * Enable FCM push notifications for the given pubkey.
     *
     * Reads the locally cached FCM token (written by
     * [NotedeckFirebaseMessagingService.onNewToken]) and registers the
     * device with the notepush server on a background coroutine.
     *
     * @param pubkeyHex The user's Nostr public key in hex format.
     * @return `true` if an FCM token was available and registration was initiated
     *         (does NOT mean server registration succeeded — that happens async,
     *         and rolls back to Disabled on failure), `false` if no token.
     */
    fun enableFcmNotifications(pubkeyHex: String): Boolean {
        val fcmToken = getSharedPreferences("notedeck_fcm", Context.MODE_PRIVATE)
            .getString("fcm_token", null)

        if (fcmToken == null) {
            Log.e(TAG, "enableFcmNotifications: no FCM token available")
            return false
        }

        // Store pubkey so disableFcmNotifications can unregister later
        getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit()
            .putString("registered_pubkey", pubkeyHex)
            .apply()

        notificationScope.launch {
            val success = notepushClient.registerDevice(pubkeyHex, fcmToken)
            if (success) {
                Log.d(TAG, "FCM registration succeeded for ${pubkeyHex.take(8)}")
            } else {
                Log.e(TAG, "FCM registration failed for ${pubkeyHex.take(8)}")
                // Only roll back to Disabled if mode is still FCM for the same pubkey.
                // The user may have switched modes while the request was in flight.
                val prefs = getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
                val currentMode = prefs.getInt(KEY_MODE, 2)
                val currentPubkey = prefs.getString("registered_pubkey", null)
                if (currentMode == 0 && currentPubkey == pubkeyHex) {
                    Log.e(TAG, "Resetting mode to Disabled after FCM registration failure")
                    setNotificationMode(2) // Disabled
                }
            }
        }

        return true
    }

    /**
     * Disable FCM push notifications.
     *
     * Unregisters from the notepush server (if a token and pubkey are
     * available). The FCM token itself is retained so re-enabling
     * doesn't require a new token from Firebase.
     */
    fun disableFcmNotifications() {
        val prefs = getSharedPreferences("notedeck_fcm", Context.MODE_PRIVATE)
        val fcmToken = prefs.getString("fcm_token", null)
        val pubkeyHex = getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .getString("registered_pubkey", null)

        if (fcmToken != null && pubkeyHex != null) {
            notificationScope.launch {
                val success = notepushClient.unregisterDevice(pubkeyHex, fcmToken)
                if (success) {
                    Log.d(TAG, "FCM unregistration succeeded")
                } else {
                    Log.e(TAG, "FCM unregistration failed")
                }
            }
        }
    }

    /**
     * Store configuration for a future native (WebSocket) notification service.
     *
     * @param pubkeyHex The user's Nostr public key in hex format.
     * @param relaysJson JSON-serialized array of relay URLs.
     */
    fun enableNativeNotifications(pubkeyHex: String, relaysJson: String) {
        getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit()
            .putString("native_pubkey", pubkeyHex)
            .putString("native_relays", relaysJson)
            .apply()

        getSharedPreferences("notedeck_prefs", Context.MODE_PRIVATE)
            .edit()
            .putBoolean("notifications_enabled", true)
            .putString("active_pubkey", pubkeyHex)
            .putString("relay_urls", relaysJson)
            .apply()

        NotificationsService.start(this)
        Log.d(TAG, "Native notification config stored for ${pubkeyHex.take(8)}")
    }

    /**
     * Clear stored native notification configuration.
     */
    fun disableNativeNotifications() {
        getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit()
            .remove("native_pubkey")
            .remove("native_relays")
            .apply()

        getSharedPreferences("notedeck_prefs", Context.MODE_PRIVATE)
            .edit()
            .putBoolean("notifications_enabled", false)
            .apply()

        NotificationsService.stop(this)
        Log.d(TAG, "Native notification config cleared")
    }

    /**
     * Check if notifications are enabled from persisted mode.
     */
    fun areNotificationsEnabled(): Boolean {
        return getNotificationMode() != 2
    }

    /**
     * Check if native notification service is currently running.
     */
    fun isNotificationServiceRunning(): Boolean {
        return NotificationsService.isServiceRunning()
    }

    /**
     * Check whether the POST_NOTIFICATIONS permission is granted.
     * On API < 33 (pre-Tiramisu), notifications are always permitted.
     */
    fun isNotificationPermissionGranted(): Boolean {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return true

        return ContextCompat.checkSelfPermission(
            this,
            Manifest.permission.POST_NOTIFICATIONS
        ) == PackageManager.PERMISSION_GRANTED
    }

    /**
     * Request the POST_NOTIFICATIONS runtime permission (API 33+).
     *
     * The result is delivered to [onRequestPermissionsResult], which
     * forwards it to Rust via [nativeOnNotificationPermissionResult].
     * On pre-33 devices the permission is implicit, so we report granted
     * immediately.
     */
    fun requestNotificationPermission() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) {
            nativeOnNotificationPermissionResult(true)
            return
        }

        ActivityCompat.requestPermissions(
            this,
            arrayOf(Manifest.permission.POST_NOTIFICATIONS),
            REQUEST_CODE_NOTIFICATION_PERMISSION
        )
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)

        if (requestCode != REQUEST_CODE_NOTIFICATION_PERMISSION) return

        val granted = grantResults.isNotEmpty() &&
            grantResults[0] == PackageManager.PERMISSION_GRANTED
        nativeOnNotificationPermissionResult(granted)
    }
}
