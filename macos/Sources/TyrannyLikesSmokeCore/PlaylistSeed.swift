/// A playlist the client just created is shown before the catalog cache has it. The seed holds
/// until the cache has loaded a playlists revision at or after the one the create reply
/// reported; from then on the cache is authoritative, so a playlist deleted since is gone.
public enum PlaylistSeed {
    public static func isActive(seedRevision: UInt64, loadedRevision: UInt64?) -> Bool {
        guard let loadedRevision else { return true }
        return loadedRevision < seedRevision
    }
}
