import SwiftUI

/// Artists: album artists with counts, a Compilations row first, and the chosen artist's
/// albums beside them with an Appears On section (spec 4.5).
struct ArtistsView: View {
    let model: NamedBrowseModel
    let player: PlayerModel
    @Bindable var navigation: NavigationModel

    var body: some View {
        NamedBrowseView(model: model, player: player, navigation: navigation)
    }
}
