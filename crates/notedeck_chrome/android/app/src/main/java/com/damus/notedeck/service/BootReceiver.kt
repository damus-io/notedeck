package com.damus.notedeck.service

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log

/**
 * Broadcast receiver that starts the NotificationsService when the device boots
 * or when the app is updated.
 */
class BootReceiver : BroadcastReceiver() {

    companion object {
        private const val TAG = "BootReceiver"
        private const val PREF_NOTIFICATIONS_ENABLED = "notifications_enabled"
    }

    override fun onReceive(context: Context, intent: Intent) {
        Log.d(TAG, "Received broadcast: ${intent.action}")

        when (intent.action) {
            Intent.ACTION_BOOT_COMPLETED,
            Intent.ACTION_MY_PACKAGE_REPLACED -> {
                startServiceIfEnabled(context)
            }
        }
    }

    private fun startServiceIfEnabled(context: Context) {
        val prefs = context.getSharedPreferences("notedeck_prefs", Context.MODE_PRIVATE)
        val notificationsEnabled = prefs.getBoolean(PREF_NOTIFICATIONS_ENABLED, false)
        val hasPubkey = !prefs.getString("active_pubkey", null).isNullOrEmpty()

        if (notificationsEnabled && hasPubkey) {
            Log.d(TAG, "Starting NotificationsService after boot")
            NotificationsService.start(context)
        } else {
            Log.d(TAG, "Notifications disabled or no pubkey configured, not starting service")
        }
    }
}
