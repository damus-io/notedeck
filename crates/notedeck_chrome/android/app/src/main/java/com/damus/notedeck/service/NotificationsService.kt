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
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
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

        // Broadcast action for other Nostr apps
        const val BROADCAST_NOSTR_EVENT = "com.shared.NOSTR"

        // Service state
        private val isRunning = AtomicBoolean(false)

        fun isServiceRunning(): Boolean = isRunning.get()

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

        fun stop(context: Context) {
            val intent = Intent(context, NotificationsService::class.java).apply {
                action = ACTION_STOP
            }
            context.startService(intent)
        }
    }

    // Coroutine scope for async operations
    private val serviceScope = CoroutineScope(Dispatchers.Main + SupervisorJob())

    // Event deduplication
    private val processedEvents = ConcurrentHashMap<String, Boolean>()

    // Wake lock to keep CPU running for WebSocket connections
    private var wakeLock: PowerManager.WakeLock? = null

    // Connected relay count for status
    private var connectedRelays = 0

    // Native methods - implemented in Rust via JNI
    private external fun nativeStartSubscriptions(pubkeyHex: String)
    private external fun nativeStopSubscriptions()
    private external fun nativeGetConnectedRelayCount(): Int

    override fun onCreate() {
        super.onCreate()
        Log.d(TAG, "Service onCreate")

        // Load native library
        try {
            System.loadLibrary("notedeck_chrome")
        } catch (e: UnsatisfiedLinkError) {
            Log.e(TAG, "Failed to load native library", e)
        }

        createNotificationChannels()
        acquireWakeLock()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        Log.d(TAG, "Service onStartCommand: ${intent?.action}")

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
        Log.d(TAG, "Service onDestroy")
        isRunning.set(false)

        try {
            nativeStopSubscriptions()
        } catch (e: Exception) {
            Log.e(TAG, "Error stopping native subscriptions", e)
        }

        releaseWakeLock()
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
            .setSmallIcon(R.drawable.ic_launcher_foreground)
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
        // Get pubkey from preferences
        val prefs = getSharedPreferences("notedeck_prefs", Context.MODE_PRIVATE)
        val pubkeyHex = prefs.getString("active_pubkey", null)

        if (pubkeyHex.isNullOrEmpty()) {
            Log.w(TAG, "No active pubkey configured, cannot start subscriptions")
            return
        }

        try {
            nativeStartSubscriptions(pubkeyHex)
            Log.d(TAG, "Started Nostr subscriptions for $pubkeyHex")
        } catch (e: Exception) {
            Log.e(TAG, "Failed to start native subscriptions", e)
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
        Log.d(TAG, "Wake lock acquired")
    }

    private fun releaseWakeLock() {
        wakeLock?.let {
            if (it.isHeld) {
                it.release()
                Log.d(TAG, "Wake lock released")
            }
        }
        wakeLock = null
    }

    /**
     * Called from native code when a new Nostr event is received.
     * This method is invoked via JNI.
     */
    @Suppress("unused") // Called from JNI
    fun onNostrEvent(eventJson: String, eventKind: Int, authorPubkey: String, authorName: String?) {
        Log.d(TAG, "Received Nostr event kind=$eventKind from $authorPubkey")

        // Deduplicate
        val eventId = eventJson.hashCode().toString() // TODO: Parse actual event ID
        if (processedEvents.putIfAbsent(eventId, true) != null) {
            return
        }

        // Show notification using the helper (async for image loading)
        serviceScope.launch {
            NotificationHelper.showNotification(
                this@NotificationsService,
                eventJson,
                eventKind,
                authorPubkey,
                authorName
            )
        }

        // Broadcast to other Nostr apps
        broadcastEvent(eventJson)
    }

    /**
     * Called from native code when relay connection status changes.
     */
    @Suppress("unused") // Called from JNI
    fun onRelayStatusChanged(connectedCount: Int) {
        connectedRelays = connectedCount
        updateServiceNotification()
    }

    private fun broadcastEvent(eventJson: String) {
        val intent = Intent(BROADCAST_NOSTR_EVENT).apply {
            putExtra("EVENT", eventJson)
        }
        sendBroadcast(intent)
    }
}
