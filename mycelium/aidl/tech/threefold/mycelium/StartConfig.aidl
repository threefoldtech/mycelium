package tech.threefold.mycelium;

/** Arguments for IMyceliumService.start(). */
parcelable StartConfig {
    /** 32-byte secret key. */
    byte[] privKey;

    /** Bootstrap peer endpoints, e.g. "tcp://1.2.3.4:9651". */
    @utf8InCpp String[] peers;

    /** Enable the embedded DNS resolver. */
    boolean enableDns;

    /** TCP listen port. */
    int tcpListenPort;

    /** QUIC listen port. Set to 0 to disable QUIC. */
    int quicListenPort;

    /** UDP port used for link-local peer discovery. */
    int peerDiscoveryPort;

    /**
     * Peer discovery mode. One of:
     *   "all"       — probe every qualifying interface (default)
     *   "disabled"  — no peer discovery
     *   "filtered"  — only interfaces whose names are in `peerDiscoveryInterfaces`
     */
    @utf8InCpp String peerDiscoveryMode;

    /** Interface name allow-list. Only consulted when peerDiscoveryMode == "filtered". */
    @utf8InCpp String[] peerDiscoveryInterfaces;

    /** Name of the TUN device. */
    @utf8InCpp String tunName;
}
