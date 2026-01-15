package com.damus.notedeck.tor;

import android.content.Context;
import android.util.Log;

import java.io.File;

/**
 * Singleton manager for Tor connectivity.
 * Automatically selects between NativeTorProvider (if available) and StubTorProvider.
 */
public class TorManager {
    private static final String TAG = "TorManager";
    private static final int DEFAULT_SOCKS_PORT = 9150;

    private static TorManager instance;

    private final TorProvider provider;
    private final Context context;
    private boolean initialized = false;

    private TorManager(Context context) {
        this.context = context.getApplicationContext();

        // Try native provider first, fall back to stub
        TorProvider testProvider = new NativeTorProvider();
        if (testProvider.isSupported()) {
            Log.i(TAG, "Using NativeTorProvider");
            this.provider = testProvider;
        } else {
            Log.i(TAG, "Native Tor not available, using StubTorProvider");
            this.provider = new StubTorProvider();
        }
    }

    /**
     * Get the singleton instance.
     *
     * @param context Application context
     * @return TorManager instance
     */
    public static synchronized TorManager getInstance(Context context) {
        if (instance == null) {
            instance = new TorManager(context);
        }
        return instance;
    }

    /**
     * Initialize and start Tor.
     *
     * @return true if Tor started successfully
     */
    public boolean start() {
        return start(DEFAULT_SOCKS_PORT);
    }

    /**
     * Initialize and start Tor on the specified port.
     *
     * @param socksPort Port for the SOCKS proxy
     * @return true if Tor started successfully
     */
    public boolean start(int socksPort) {
        if (!provider.isSupported()) {
            Log.w(TAG, "Tor is not supported on this build");
            return false;
        }

        if (!initialized) {
            File cacheDir = new File(context.getCacheDir(), "tor");
            File stateDir = new File(context.getFilesDir(), "tor");

            // Create directories
            cacheDir.mkdirs();
            stateDir.mkdirs();

            Log.i(TAG, "Initializing Tor (cache: " + cacheDir + ", state: " + stateDir + ")");

            if (!provider.initialize(cacheDir.getAbsolutePath(), stateDir.getAbsolutePath())) {
                Log.e(TAG, "Failed to initialize Tor");
                return false;
            }

            initialized = true;
        }

        Log.i(TAG, "Starting SOCKS proxy on port " + socksPort);
        return provider.startSocksProxy(socksPort);
    }

    /**
     * Stop Tor.
     */
    public void stop() {
        provider.stop();
    }

    /**
     * Get the SOCKS proxy address.
     *
     * @return "127.0.0.1:port" if running, null otherwise
     */
    public String getSocksProxy() {
        return provider.getSocksProxy();
    }

    /**
     * Check if Tor is supported.
     *
     * @return true if Tor support is available
     */
    public boolean isSupported() {
        return provider.isSupported();
    }

    /**
     * Check if Tor is running.
     *
     * @return true if Tor is initialized and proxy is running
     */
    public boolean isRunning() {
        return provider.getSocksProxy() != null;
    }

    /**
     * Set a log callback.
     *
     * @param callback The callback, or null to disable
     */
    public void setLogCallback(TorLogCallback callback) {
        provider.setLogCallback(callback);
    }
}
