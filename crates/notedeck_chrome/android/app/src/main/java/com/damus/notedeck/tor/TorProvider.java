package com.damus.notedeck.tor;

/**
 * Interface for Tor connectivity providers.
 * Allows swapping between native Arti implementation and stub for builds without Tor.
 */
public interface TorProvider {
    /**
     * Initialize the Tor client with data directories.
     *
     * @param cacheDir Directory for cached consensus and descriptor data
     * @param stateDir Directory for guard state and other persistent state
     * @return true if initialization succeeded
     */
    boolean initialize(String cacheDir, String stateDir);

    /**
     * Start the SOCKS5 proxy on the specified port.
     *
     * @param port Port number for the SOCKS proxy (e.g., 9150)
     * @return true if proxy started successfully
     */
    boolean startSocksProxy(int port);

    /**
     * Stop the SOCKS5 proxy.
     */
    void stop();

    /**
     * Get the SOCKS proxy address if running.
     *
     * @return "127.0.0.1:port" if running, null otherwise
     */
    String getSocksProxy();

    /**
     * Check if Tor is initialized and ready.
     *
     * @return true if initialized
     */
    boolean isInitialized();

    /**
     * Check if Tor is supported on this build.
     *
     * @return true if Tor support is available
     */
    boolean isSupported();

    /**
     * Set a callback for receiving log messages.
     *
     * @param callback The callback to receive log messages, or null to disable
     */
    void setLogCallback(TorLogCallback callback);
}
