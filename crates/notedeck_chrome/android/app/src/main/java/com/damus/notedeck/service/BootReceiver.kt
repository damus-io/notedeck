package com.damus.notedeck.service

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log

/**
 * Broadcast receiver that starts the NotificationsService when the device boots
 * or when the app is updated.
 *
 * ## Known Limitation: Single-Account Only
 *
 * Currently only supports notifications for one account (the `active_pubkey` in preferences).
 * Multi-account users will only receive notifications for their last-active account after reboot.
 *
 * Full multi-account support would require:
 * 1. Storing a set of pubkeys with notifications enabled (not just active_pubkey)
 * 2. NotificationsService subscribing to events for all enabled accounts
 * 3. Rust backend handling multiple subscription filters
 *
 * See: notedeck-e12 for tracking this limitation.
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
