import SwiftUI

/// Genres: a genre list and the chosen genre's albums beside it (spec 4.5).
struct GenresView: View {
    let model: NamedBrowseModel
    let player: PlayerModel
    @Bindable var navigation: NavigationModel

    var body: some View {
        NamedBrowseView(model: model, player: player, navigation: navigation)
    }
}
