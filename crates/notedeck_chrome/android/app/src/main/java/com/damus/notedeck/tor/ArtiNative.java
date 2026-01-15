package com.damus.notedeck.tor;

/**
 * JNI bindings to the Arti Tor client native library.
 */
public class ArtiNative {
    private static boolean libraryLoaded = false;
    private static String loadError = null;

    static {
        try {
            System.loadLibrary("arti_android");
            libraryLoaded = true;
        } catch (UnsatisfiedLinkError e) {
            loadError = e.getMessage();
            libraryLoaded = false;
        }
    }

    /**
     * Check if the native library was loaded successfully.
     */
    public static boolean isLibraryLoaded() {
        return libraryLoaded;
    }

    /**
     * Get the error message if library loading failed.
     */
    public static String getLoadError() {
        return loadError;
    }

    /**
     * Initialize the Arti Tor client with the specified data directories.
     *
     * @param cacheDir Directory for cached consensus and descriptor data
     * @param stateDir Directory for guard state and other persistent state
     * @return true if initialization succeeded
     */
    public static native boolean initialize(String cacheDir, String stateDir);

    /**
     * Start the SOCKS5 proxy on the specified port.
     *
     * @param port Port number for the SOCKS proxy
     * @return true if proxy started successfully
     */
    public static native boolean startSocksProxy(int port);

    /**
     * Stop the SOCKS5 proxy.
     */
    public static native void stop();

    /**
     * Get the current SOCKS proxy port.
     *
     * @return The port number, or -1 if not running
     */
    public static native int getSocksPort();

    /**
     * Check if the Tor client is initialized and ready.
     *
     * @return true if initialized
     */
    public static native boolean isInitialized();

    /**
     * Get the Arti version string.
     *
     * @return Version string
     */
    public static native String getVersion();

    /**
     * Set the log callback for receiving log messages.
     *
     * @param callback The callback to receive logs, or null to disable
     */
    public static native void setLogCallback(TorLogCallback callback);
}
