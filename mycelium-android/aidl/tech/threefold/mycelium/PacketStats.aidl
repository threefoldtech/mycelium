package tech.threefold.mycelium;

import tech.threefold.mycelium.PacketStatEntry;

parcelable PacketStats {
    List<PacketStatEntry> bySource;
    List<PacketStatEntry> byDestination;
}
