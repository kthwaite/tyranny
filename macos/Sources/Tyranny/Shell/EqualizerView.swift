import AppKit
import SwiftUI
import TyrannyCore

struct EqualizerView: View {
    let model: EqualizerModel

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                Toggle("Enable Equalizer", isOn: Binding(
                    get: { model.settings.enabled }, set: { model.setEnabled($0) }))
                    .accessibilityIdentifier("equalizer-enabled")
                Spacer()
                Picker("Preset", selection: Binding(
                    get: { model.settings.preset?.rawValue ?? "Custom" },
                    set: { if let preset = EqualizerPreset(rawValue: $0) { model.selectPreset(preset) } })) {
                    ForEach(EqualizerPreset.allCases, id: \.rawValue) { preset in
                        Text(preset.rawValue).tag(preset.rawValue)
                    }
                    Text("Custom").tag("Custom").disabled(true)
                }
                .frame(width: 235)
                .accessibilityIdentifier("equalizer-preset")
            }
            Text(model.settings.enabled ? "Equalizer is enabled." : "Bypassed — your curve is retained.")
                .font(.callout).foregroundStyle(.secondary)
                .accessibilityIdentifier("equalizer-status")
            gainRow("Preamp", identifier: "equalizer-preamp", range: -24...12,
                    value: Binding(get: { model.settings.preamp_db }, set: { model.setPreamp($0) }))
            Divider()
            VStack(spacing: 10) {
                ForEach(EqualizerSettings.labels.indices, id: \.self) { index in
                    gainRow(EqualizerSettings.labels[index], identifier: "equalizer-band-\(index)", range: -12...12,
                            value: Binding(get: { model.settings.bands_db[index] }, set: { model.setBand(index, gain: $0) }))
                }
            }
            Divider()
            Text("Boosts can clip. Lower the preamp to leave headroom; presets include attenuation, but do not guarantee clip-free playback. Bypass retains all gains.")
                .font(.caption).foregroundStyle(.secondary)
            Text("Changes affect newly processed audio. Already buffered audio is retained: allow about 1.4 seconds, plus decoder and device buffering. Changes while paused take effect after that buffer plays on resume.")
                .font(.caption).foregroundStyle(.secondary)
                .accessibilityIdentifier("equalizer-buffer-note")
            if !model.canEdit {
                Text("Connect to the server to edit the equalizer.")
                    .foregroundStyle(.secondary).accessibilityIdentifier("equalizer-disconnected")
            }
            if let error = model.error {
                Text(error).foregroundStyle(.red).font(.callout)
                    .textSelection(.enabled).accessibilityIdentifier("equalizer-error")
            }
        }
        .padding(24)
        .frame(width: 600)
        .disabled(!model.canEdit)
    }

    private func gainRow(_ label: String, identifier: String, range: ClosedRange<Double>, value: Binding<Double>) -> some View {
        HStack(spacing: 12) {
            Text(label).frame(width: 72, alignment: .trailing)
            EqualizerGainSlider(value: value, range: range, enabled: model.canEdit,
                                identifier: identifier, label: "\(label) gain in decibels")
            Text(String(format: "%+.1f dB", value.wrappedValue))
                .monospacedDigit().frame(width: 72, alignment: .trailing)
                .accessibilityIdentifier(identifier + "-value")
        }
    }
}

/// Reuses the transport slider's AX action path, with half-decibel VoiceOver steps.
private struct EqualizerGainSlider: NSViewRepresentable {
    @Binding var value: Double
    let range: ClosedRange<Double>
    let enabled: Bool
    let identifier: String
    let label: String

    func makeCoordinator() -> Coordinator { Coordinator(value: $value) }

    func makeNSView(context: Context) -> AccessibleSlider {
        let slider = AccessibleSlider()
        slider.minValue = range.lowerBound
        slider.maxValue = range.upperBound
        slider.isContinuous = true
        slider.accessibilityStep = 0.5
        slider.target = context.coordinator
        slider.action = #selector(Coordinator.changed(_:))
        slider.setAccessibilityIdentifier(identifier)
        slider.setAccessibilityLabel(label)
        slider.toolTip = label
        return slider
    }

    func updateNSView(_ slider: AccessibleSlider, context: Context) {
        context.coordinator.value = $value
        slider.isEnabled = enabled
        if slider.doubleValue != value { slider.doubleValue = value }
    }

    @MainActor final class Coordinator: NSObject {
        var value: Binding<Double>
        init(value: Binding<Double>) { self.value = value }
        @objc func changed(_ slider: NSSlider) {
            value.wrappedValue = (slider.doubleValue * 2).rounded() / 2
        }
    }
}
