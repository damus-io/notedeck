package com.damus.notedeck.tor;

/**
 * Callback interface for receiving Tor log messages.
 */
public interface TorLogCallback {
    /**
     * Called when a log message is received from the Tor client.
     *
     * @param message The log message
     */
    void onLog(String message);
}
