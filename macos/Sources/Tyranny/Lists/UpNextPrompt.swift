import SwiftUI
import TyrannyCore

/// "Clear Up Next?": asked when a play would replace entries still to come and the policy is Ask.
/// Clear replaces them; Keep puts the new entries next and keeps the rest.
struct UpNextPrompt: View {
    let activation: ActivationModel
    @State private var remember = false

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Clear Up Next?")
                .font(.headline)
            Text("Up Next still has tracks to play. Clear them, or keep them after the tracks you chose.")
                .fixedSize(horizontal: false, vertical: true)
            Toggle("Don't ask again", isOn: $remember)
                .accessibilityIdentifier("up-next-prompt-remember")
            HStack {
                Spacer()
                Button("Keep") { activation.answer(.keep, remember: remember) }
                    .accessibilityIdentifier("up-next-prompt-keep")
                Button("Clear") { activation.answer(.clear, remember: remember) }
                    .keyboardShortcut(.defaultAction)
                    .accessibilityIdentifier("up-next-prompt-clear")
            }
        }
        .padding(20)
        .frame(width: 380)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("up-next-prompt")
    }
}
