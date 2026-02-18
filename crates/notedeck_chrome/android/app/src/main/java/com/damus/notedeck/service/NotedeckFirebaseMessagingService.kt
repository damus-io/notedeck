package com.damus.notedeck.service

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Build
import android.util.Log
import androidx.core.app.NotificationCompat
import com.damus.notedeck.BuildConfig
import com.damus.notedeck.MainActivity
import com.damus.notedeck.R
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.runBlocking
import java.util.concurrent.atomic.AtomicInteger

/**
 * Firebase Cloud Messaging service for receiving push notifications from notepush server.
 *
 * The notepush server sends FCM messages with a data payload containing:
 * - nostr_event: JSON string of the Nostr event that triggered the notification
 */
class NotedeckFirebaseMessagingService : FirebaseMessagingService() {

    companion object {
        private const val TAG = "NotedeckFCM"
        private const val CHANNEL_ID = "notedeck_notifications"
        private const val CHANNEL_NAME = "Notedeck Notifications"
        private val notificationIdCounter = AtomicInteger(0)

        // Must match NotificationMode enum in Rust (platform/mod.rs)
        const val MODE_FCM = 0
        const val MODE_NATIVE = 1
        const val MODE_DISABLED = 2
    }

    override fun onCreate() {
        super.onCreate()
        createNotificationChannel()
    }

    /**
     * Called when a new FCM token is generated.
     * This happens on first app start and when token is rotated.
     */
    override fun onNewToken(token: String) {
        super.onNewToken(token)
        // Redact token in logs to avoid exposing sensitive credentials
        if (BuildConfig.DEBUG) {
            val redacted = if (token.length > 8) "...${token.takeLast(4)}" else "[redacted]"
            Log.d(TAG, "New FCM token (${token.length} chars): $redacted")
        }

        // Store token locally
        getSharedPreferences("notedeck_fcm", Context.MODE_PRIVATE)
            .edit()
            .putString("fcm_token", token)
            .apply()

        // Notify Rust layer of new token
        try {
            nativeOnFcmTokenRefreshed(token)
        } catch (e: UnsatisfiedLinkError) {
            Log.e(TAG, "Native library not available for token refresh", e)
        }

        // Re-register with notepush if FCM mode is active.
        // onNewToken runs on a Firebase worker thread so blocking is safe.
        val prefs = getSharedPreferences("notedeck_notifications", Context.MODE_PRIVATE)
        val mode = prefs.getInt("notification_mode", MODE_DISABLED)
        val pubkey = prefs.getString("registered_pubkey", null)
        if (mode == MODE_FCM && pubkey != null) {
            val client = NotepushClient()
            runBlocking(Dispatchers.IO) {
                val success = client.registerDevice(pubkey, token)
                if (success) {
                    Log.d(TAG, "Re-registered with new FCM token for ${pubkey.take(8)}")
                } else {
                    Log.e(TAG, "Failed to re-register with new FCM token")
                }
            }
        }
    }

    /**
     * Called when an FCM message is received.
     * notepush sends data-only messages with nostr_event in the payload.
     */
    override fun onMessageReceived(message: RemoteMessage) {
        super.onMessageReceived(message)
        Log.d(TAG, "FCM message received from: ${message.from}")

        // Drop messages if FCM mode is no longer active.  This handles the
        // case where server unregistration failed (fire-and-forget) but the
        // user has locally disabled FCM or switched to Native mode.
        val mode = getSharedPreferences("notedeck_notifications", Context.MODE_PRIVATE)
            .getInt("notification_mode", MODE_DISABLED)
        if (mode != MODE_FCM) {
            Log.d(TAG, "Ignoring FCM message — notification mode is $mode, not FCM")
            return
        }

        // Extract Nostr event from data payload
        val nostrEventJson = message.data["nostr_event"]
        if (nostrEventJson != null) {
            // Only log event content in debug builds to avoid leaking user PII
            if (BuildConfig.DEBUG) {
                Log.d(TAG, "Received Nostr event: ${nostrEventJson.take(100)}...")
            }
            handleNostrEvent(nostrEventJson)
        } else {
            // Fallback: show notification from FCM notification payload
            message.notification?.let { notification ->
                showNotification(
                    title = notification.title ?: "New Activity",
                    body = notification.body ?: ""
                )
            }
        }
    }

    /**
     * Process incoming Nostr event and create appropriate notification.
     */
    private fun handleNostrEvent(eventJson: String) {
        // Pass to Rust for processing - it will call back with notification details
        val result = try {
            nativeProcessNostrEvent(eventJson)
        } catch (e: UnsatisfiedLinkError) {
            Log.e(TAG, "Native library not available for event processing", e)
            return
        }

        if (result != null) {
            showNotification(
                title = result.title,
                body = result.body,
                eventId = result.eventId,
                eventKind = result.eventKind,
                authorPubkey = result.authorPubkey
            )
        }
    }

    /**
     * Display a system notification to the user.
     *
     * @param title Notification title (e.g., "New mention", "Someone zapped you")
     * @param body Notification body text
     * @param eventId Optional Nostr event ID for deep-linking when tapped
     */
    private fun showNotification(
        title: String,
        body: String,
        eventId: String? = null,
        eventKind: Int = 0,
        authorPubkey: String? = null
    ) {
        val intent = Intent(this, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP
            eventId?.let { putExtra("event_id", it) }
            putExtra("event_kind", eventKind)
            authorPubkey?.let { putExtra("author_pubkey", it) }
        }

        val notificationId = notificationIdCounter.getAndIncrement()

        val pendingIntent = PendingIntent.getActivity(
            this,
            notificationId,
            intent,
            PendingIntent.FLAG_ONE_SHOT or PendingIntent.FLAG_IMMUTABLE
        )

        val notification = NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.mipmap.ic_launcher)
            .setContentTitle(title)
            .setContentText(body)
            .setAutoCancel(true)
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setContentIntent(pendingIntent)
            .build()

        val notificationManager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        notificationManager.notify(notificationId, notification)
    }

    /**
     * Create notification channel for Android O+.
     */
    private fun createNotificationChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                CHANNEL_ID,
                CHANNEL_NAME,
                NotificationManager.IMPORTANCE_HIGH
            ).apply {
                description = "Notifications from Nostr network"
                enableVibration(true)
            }

            val notificationManager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            notificationManager.createNotificationChannel(channel)
        }
    }

    /**
     * Data class for notification details returned from Rust.
     */
    data class NotificationResult(
        val title: String,
        val body: String,
        val eventId: String?,
        val eventKind: Int,
        val authorPubkey: String?
    )

    /**
     * Notifies Rust layer of FCM token refresh.
     * Implemented in android.rs - stores token for later notepush registration.
     */
    private external fun nativeOnFcmTokenRefreshed(token: String)

    /**
     * Processes a Nostr event JSON and returns notification details.
     * Implemented in android.rs - parses event kind and content.
     *
     * @param eventJson Raw JSON of the Nostr event from notepush
     * @return NotificationResult with title/body, or null if parsing fails
     */
    private external fun nativeProcessNostrEvent(eventJson: String): NotificationResult?

    init {
        System.loadLibrary("notedeck_chrome")
    }
}
