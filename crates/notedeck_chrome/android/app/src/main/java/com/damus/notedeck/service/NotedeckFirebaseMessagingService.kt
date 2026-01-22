package com.damus.notedeck.service

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Build
import android.util.Log
import androidx.core.app.NotificationCompat
import com.damus.notedeck.MainActivity
import com.damus.notedeck.R
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage
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
        Log.d(TAG, "New FCM token: $token")

        // Store token locally
        getSharedPreferences("notedeck_fcm", Context.MODE_PRIVATE)
            .edit()
            .putString("fcm_token", token)
            .apply()

        // Notify Rust layer of new token
        nativeOnFcmTokenRefreshed(token)
    }

    /**
     * Called when an FCM message is received.
     * notepush sends data-only messages with nostr_event in the payload.
     */
    override fun onMessageReceived(message: RemoteMessage) {
        super.onMessageReceived(message)
        Log.d(TAG, "FCM message received from: ${message.from}")

        // Extract Nostr event from data payload
        val nostrEventJson = message.data["nostr_event"]
        if (nostrEventJson != null) {
            Log.d(TAG, "Received Nostr event: ${nostrEventJson.take(100)}...")
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
        val result = nativeProcessNostrEvent(eventJson)

        if (result != null) {
            showNotification(
                title = result.title,
                body = result.body,
                eventId = result.eventId
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
        eventId: String? = null
    ) {
        val intent = Intent(this, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP
            eventId?.let { putExtra("event_id", it) }
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
        val eventId: String?
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
