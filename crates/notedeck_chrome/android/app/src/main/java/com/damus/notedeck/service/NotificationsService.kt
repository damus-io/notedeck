package com.damus.notedeck.service

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import com.damus.notedeck.MainActivity
import com.damus.notedeck.R
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch

/**
 * Foreground service that maintains WebSocket connections to Nostr relays
 * for real-time push notifications without requiring Google Play Services.
 *
 * This is a Pokey-style implementation where the app maintains direct
 * relay connections rather than relying on a push notification server.
 */
class NotificationsService : Service() {

    companion object {
        private const val TAG = "NotificationsService"

        // Notification channels
        const val CHANNEL_SERVICE = "notedeck_service"
        const val CHANNEL_NOTIFICATIONS = "notedeck_notifications"
        const val CHANNEL_DMS = "notedeck_dms"
        const val CHANNEL_ZAPS = "notedeck_zaps"

        // Notification IDs
        const val NOTIFICATION_ID_SERVICE = 1
        const val NOTIFICATION_ID_BASE = 1000

        // Intent actions
        const val ACTION_START = "com.damus.notedeck.START_NOTIFICATIONS"
        const val ACTION_STOP = "com.damus.notedeck.STOP_NOTIFICATIONS"

        // Broadcast action for other Nostr apps (non-DM events only)
        const val BROADCAST_NOSTR_EVENT = "com.damus.notedeck.NOSTR_EVENT"
        // Permission required to receive broadcasts
        const val BROADCAST_PERMISSION = "com.damus.notedeck.permission.RECEIVE_NOSTR_EVENTS"

        // Event kinds that should NOT be broadcast (privacy-sensitive)
        private val PRIVATE_EVENT_KINDS = setOf(4, 1059) // Legacy DM, Gift-wrapped DM

        // Max size for dedup cache (LRU eviction)
        private const val MAX_DEDUP_CACHE = 1000

        // Service state
        private val isRunning = AtomicBoolean(false)

        @JvmStatic
        fun isServiceRunning(): Boolean = isRunning.get()

        @JvmStatic
        fun start(context: Context) {
            val intent = Intent(context, NotificationsService::class.java).apply {
                action = ACTION_START
            }
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                context.startForegroundService(intent)
            } else {
                context.startService(intent)
            }
        }

        @JvmStatic
        fun stop(context: Context) {
            val intent = Intent(context, NotificationsService::class.java).apply {
                action = ACTION_STOP
            }
            context.startService(intent)
        }
    }

    // Coroutine scope for async operations
    private var serviceScope: CoroutineScope? = null

    // Event deduplication - bounded LRU cache to prevent memory leaks
    // Synchronized access required since it's accessed from JNI callback thread
    private val processedEvents = object : LinkedHashMap<String, Boolean>(100, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, Boolean>?): Boolean {
            return size > MAX_DEDUP_CACHE
        }
    }

    // Wake lock to keep CPU running for WebSocket connections
    private var wakeLock: PowerManager.WakeLock? = null

    // Connected relay count for status
    private var connectedRelays = 0

    // Native methods - implemented in Rust via JNI
    private external fun nativeStartSubscriptions(pubkeyHex: String, relayUrlsJson: String)
    private external fun nativeStopSubscriptions()
    private external fun nativeGetConnectedRelayCount(): Int

    override fun onCreate() {
        super.onCreate()
        Log.i(TAG, "Service onCreate")

        // Load native library
        try {
            System.loadLibrary("notedeck_chrome")
        } catch (e: UnsatisfiedLinkError) {
            Log.e(TAG, "Failed to load native library", e)
        }

        serviceScope = CoroutineScope(Dispatchers.Main + SupervisorJob())
        createNotificationChannels()
        acquireWakeLock()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        Log.i(TAG, "Service onStartCommand: ${intent?.action}")

        when (intent?.action) {
            ACTION_STOP -> {
                stopSelf()
                return START_NOT_STICKY
            }
            ACTION_START, null -> {
                startForegroundWithNotification()
                startNostrSubscriptions()
            }
        }

        return START_STICKY
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onDestroy() {
        Log.i(TAG, "Service onDestroy")
        isRunning.set(false)

        try {
            nativeStopSubscriptions()
        } catch (e: Exception) {
            Log.e(TAG, "Error stopping native subscriptions", e)
        }

        serviceScope?.cancel()
        serviceScope = null
        releaseWakeLock()

        synchronized(processedEvents) {
            processedEvents.clear()
        }

        super.onDestroy()
    }

    private fun createNotificationChannels() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val notificationManager = getSystemService(NotificationManager::class.java)

            // Service channel (low importance - just shows we're running)
            val serviceChannel = NotificationChannel(
                CHANNEL_SERVICE,
                "Background Service",
                NotificationManager.IMPORTANCE_LOW
            ).apply {
                description = "Shows when Notedeck is listening for notifications"
                setShowBadge(false)
            }

            // Notifications channel (mentions, replies, reactions)
            val notificationsChannel = NotificationChannel(
                CHANNEL_NOTIFICATIONS,
                "Notifications",
                NotificationManager.IMPORTANCE_DEFAULT
            ).apply {
                description = "Mentions, replies, and reactions"
            }

            // DMs channel (higher importance)
            val dmsChannel = NotificationChannel(
                CHANNEL_DMS,
                "Direct Messages",
                NotificationManager.IMPORTANCE_HIGH
            ).apply {
                description = "Private messages"
            }

            // Zaps channel
            val zapsChannel = NotificationChannel(
                CHANNEL_ZAPS,
                "Zaps",
                NotificationManager.IMPORTANCE_DEFAULT
            ).apply {
                description = "Lightning zap notifications"
            }

            notificationManager.createNotificationChannels(
                listOf(serviceChannel, notificationsChannel, dmsChannel, zapsChannel)
            )
        }
    }

    private fun startForegroundWithNotification() {
        val notification = createServiceNotification()

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            ServiceCompat.startForeground(
                this,
                NOTIFICATION_ID_SERVICE,
                notification,
                ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE
            )
        } else {
            startForeground(NOTIFICATION_ID_SERVICE, notification)
        }

        isRunning.set(true)
    }

    private fun createServiceNotification(): Notification {
        val pendingIntent = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE
        )

        val stopIntent = PendingIntent.getService(
            this,
            0,
            Intent(this, NotificationsService::class.java).apply {
                action = ACTION_STOP
            },
            PendingIntent.FLAG_IMMUTABLE
        )

        return NotificationCompat.Builder(this, CHANNEL_SERVICE)
            .setContentTitle("Notedeck")
            .setContentText("Listening for Nostr events ($connectedRelays relays)")
            .setSmallIcon(R.mipmap.damusfg)
            .setContentIntent(pendingIntent)
            .addAction(0, "Stop", stopIntent)
            .setOngoing(true)
            .setSilent(true)
            .build()
    }

    private fun updateServiceNotification() {
        val notificationManager = getSystemService(NotificationManager::class.java)
        notificationManager.notify(NOTIFICATION_ID_SERVICE, createServiceNotification())
    }

    private fun startNostrSubscriptions() {
        // Get pubkey and relay URLs from preferences
        val prefs = getSharedPreferences("notedeck_prefs", Context.MODE_PRIVATE)
        val pubkeyHex = prefs.getString("active_pubkey", null)
        val relayUrlsJson = prefs.getString("relay_urls", "[]") ?: "[]"

        if (pubkeyHex.isNullOrEmpty()) {
            Log.w(TAG, "No active pubkey configured, cannot start subscriptions")
            return
        }

        // Run native subscriptions on IO thread to avoid ANR
        serviceScope?.launch(Dispatchers.IO) {
            try {
                nativeStartSubscriptions(pubkeyHex, relayUrlsJson)
                Log.i(TAG, "Started Nostr subscriptions for $pubkeyHex with relays: $relayUrlsJson")
            } catch (e: Exception) {
                Log.e(TAG, "Failed to start native subscriptions", e)
            }
        }
    }

    private fun acquireWakeLock() {
        val powerManager = getSystemService(Context.POWER_SERVICE) as PowerManager
        wakeLock = powerManager.newWakeLock(
            PowerManager.PARTIAL_WAKE_LOCK,
            "notedeck:NotificationsService"
        ).apply {
            acquire()
        }
        Log.i(TAG, "Wake lock acquired")
    }

    private fun releaseWakeLock() {
        wakeLock?.let {
            if (it.isHeld) {
                it.release()
                Log.i(TAG, "Wake lock released")
            }
        }
        wakeLock = null
    }

    /**
     * Called from native code when a new Nostr event is received.
     * This method is invoked via JNI with structured data (no JSON parsing needed).
     *
     * @param eventId The 64-char hex event ID (for deduplication)
     * @param eventKind The Nostr event kind (1=note, 4=DM, 7=reaction, etc.)
     * @param authorPubkey The 64-char hex pubkey of the event author
     * @param content The event content (already extracted from JSON in Rust)
     * @param authorName Optional display name of the author
     * @param authorPictureUrl Optional profile picture URL of the author
     * @param zapAmountSats Zap amount in satoshis (-1 if not a zap or amount unknown)
     * @param rawJson Full event JSON from Rust (properly escaped, includes all fields)
     */
    @Suppress("unused") // Called from JNI
    fun onNostrEvent(
        eventId: String,
        eventKind: Int,
        authorPubkey: String,
        content: String,
        authorName: String?,
        authorPictureUrl: String?,
        zapAmountSats: Long,
        rawJson: String
    ) {
        Log.i(TAG, "Received Nostr event kind=$eventKind id=${eventId.take(8)} from ${authorPubkey.take(8)}")

        // Deduplicate using event ID directly (no JSON parsing needed)
        synchronized(processedEvents) {
            if (processedEvents.containsKey(eventId)) {
                Log.i(TAG, "Skipping duplicate event id=${eventId.take(8)}")
                return
            }
            processedEvents[eventId] = true
        }

        Log.i(TAG, "Event is new, showing notification...")

        // Show notification using the helper (async for image loading)
        val scope = serviceScope
        if (scope == null) {
            Log.e(TAG, "serviceScope is null! Cannot show notification")
            return
        }

        scope.launch {
            try {
                Log.i(TAG, "Calling NotificationHelper.showNotification...")
                NotificationHelper.showNotification(
                    this@NotificationsService,
                    eventId,
                    eventKind,
                    authorPubkey,
                    content,
                    authorName,
                    authorPictureUrl,
                    if (zapAmountSats >= 0) zapAmountSats else null
                )
                Log.i(TAG, "NotificationHelper.showNotification completed")
            } catch (e: Exception) {
                Log.e(TAG, "Error showing notification", e)
            }
        }

        // Broadcast to other Nostr apps (but NOT DMs for privacy)
        if (eventKind !in PRIVATE_EVENT_KINDS) {
            broadcastEvent(eventId, eventKind, authorPubkey, rawJson)
        }
    }

    /**
     * Broadcast event to other Nostr apps.
     * Uses a permission to prevent unauthorized apps from receiving events.
     * DMs are never broadcast for privacy.
     * Uses full event JSON from Rust (properly escaped, includes all fields).
     */
    private fun broadcastEvent(eventId: String, eventKind: Int, authorPubkey: String, rawJson: String) {
        val intent = Intent(BROADCAST_NOSTR_EVENT).apply {
            putExtra("EVENT", rawJson)
            putExtra("EVENT_ID", eventId)
            putExtra("EVENT_KIND", eventKind)
            putExtra("AUTHOR_PUBKEY", authorPubkey)
        }
        sendBroadcast(intent, BROADCAST_PERMISSION)
    }

    /**
     * Called from native code when relay connection status changes.
     */
    @Suppress("unused") // Called from JNI
    fun onRelayStatusChanged(connectedCount: Int) {
        connectedRelays = connectedCount
        updateServiceNotification()
    }
}
