package tech.threefold.mycelium;

import tech.threefold.mycelium.NodeInfo;
import tech.threefold.mycelium.PacketStatEntry;
import tech.threefold.mycelium.PacketStats;
import tech.threefold.mycelium.PeerInfo;
import tech.threefold.mycelium.QueriedSubnet;
import tech.threefold.mycelium.Route;

interface IMyceliumService {
    /** Start the node. peers = bootstrap endpoints e.g. "tcp://1.2.3.4:9651". privKey = 32 bytes. */
    boolean start(in String[] peers, in byte[] privKey, boolean enableDns);

    void stop();

    boolean isRunning();

    NodeInfo getNodeInfo();

    /** Returns the hex-encoded public key for the given IPv6 address, or empty string if unknown. */
    String getPublicKeyFromIp(String ip);

    List<PeerInfo> getPeers();

    /** Returns true if the peer was added, false if it already exists. */
    boolean addPeer(String endpoint);

    /** Returns true if the peer was removed, false if it was not found. */
    boolean removePeer(String endpoint);

    List<Route> getSelectedRoutes();

    List<Route> getFallbackRoutes();

    List<QueriedSubnet> getQueriedSubnets();

    PacketStats getPacketStats();

    /** Returns 32 random key bytes. */
    byte[] generateSecretKey();

    /** Derives the node's IPv6 address string from a 32-byte secret key. */
    String addressFromSecretKey(in byte[] key);

    boolean startProxyProbe();
    boolean stopProxyProbe();
    List<String> listProxies();

    /** remote = "" for auto-select, or "ip:port". Returns bound address on success. */
    String proxyConnect(String remote);
    void proxyDisconnect();
}
