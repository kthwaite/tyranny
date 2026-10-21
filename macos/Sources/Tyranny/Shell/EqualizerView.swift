import AppKit
import SwiftUI
import TyrannyCore

struct EqualizerView: View {
    @Environment(\.themeColours) private var colours
    @Bindable var model: EqualizerModel
    @State private var presetName = ""
    @State private var selectedPreset: Data?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                HStack {
                    Toggle("Enable Equalizer", isOn: Binding(get: { model.settings.enabled }, set: model.setEnabled))
                        .accessibilityIdentifier("equalizer-enabled")
                    Spacer()
                    Picker("Factory preset", selection: Binding(
                        get: { model.settings.preset?.rawValue ?? "Custom" },
                        set: { if let preset = EqualizerPreset(rawValue: $0) { model.selectPreset(preset) } })) {
                        ForEach(EqualizerPreset.allCases, id: \.rawValue) { Text($0.rawValue).tag($0.rawValue) }
                        Text("Custom").tag("Custom").disabled(true)
                    }
                    .frame(width: 260).accessibilityIdentifier("equalizer-preset")
                    Button("Reset", action: model.reset).accessibilityIdentifier("equalizer-reset")
                }
                Text(model.settings.enabled ? "Equalizer is enabled." : "Bypassed — the graph previews your retained curve, not the output.")
                    .font(.callout).foregroundStyle(.secondary).accessibilityIdentifier("equalizer-status")
                HStack {
                    Text("Solid: combined + preamp").foregroundStyle(Color.accentColor)
                    Text("Dashed: selected band").foregroundStyle(.orange)
                    Spacer()
                    Text("Drag points; Return applies numeric edits.").foregroundStyle(.secondary)
                }.font(.caption)
                EqualizerResponseGraph(model: model)
                    .frame(height: 230)
                HStack {
                    Text("Bands: \(model.settings.bands.count)/16").monospacedDigit()
                    Spacer()
                    Button("Add Band", action: model.addBand)
                        .disabled(model.settings.bands.count == 16).accessibilityIdentifier("equalizer-add-band")
                    Button("Remove Band", action: model.removeBand)
                        .disabled(!model.settings.bands.indices.contains(model.selectedBand))
                        .accessibilityIdentifier("equalizer-remove-band")
                }
                if !model.settings.bands.isEmpty { bandRows }
                if model.settings.bands.indices.contains(model.selectedBand) {
                    bandEditor
                } else {
                    Text("No filters. Add a band or choose a preset to shape the response.")
                        .foregroundStyle(.secondary)
                }
                Divider()
                HStack {
                    Text("Preamp").frame(width: 65, alignment: .leading)
                    EqualizerGainSlider(value: Binding(get: { model.settings.preamp_db }, set: model.setPreamp),
                                        range: -96...12, enabled: model.canEdit,
                                        identifier: "equalizer-preamp", label: "Preamp gain in decibels")
                    EqualizerNumberField(label: "Preamp dB", identifier: "equalizer-preamp-input", range: -96...12,
                                         value: Binding(get: { model.settings.preamp_db }, set: model.setPreamp))
                        .frame(width: 190)
                }
                headroom
                Divider()
                presetEditor
                Text("Changes affect newly rendered audio. Audio already buffered continues with the previous curve; pause/resume does not flush it. Filters transition briefly to avoid abrupt changes.")
                    .font(.caption).foregroundStyle(.secondary).accessibilityIdentifier("equalizer-buffer-note")
                if !model.canEdit {
                    Text("Connect to the server to edit the equalizer.")
                        .foregroundStyle(.secondary).accessibilityIdentifier("equalizer-disconnected")
                }
                if let error = model.error {
                    Text(error).foregroundStyle(colours.error).font(.callout)
                        .textSelection(.enabled).accessibilityIdentifier("equalizer-error")
                }
            }
            .padding(22)
            .disabled(!model.canEdit)
        }
        .frame(width: 880, height: 800)
    }

    private var bandRows: some View {
        let bands = model.settings.bands
        return ScrollViewReader { scroll in
            ScrollView {
                VStack(spacing: 2) {
                    ForEach(bands.indices, id: \.self) { index in
                        let band = bands[index]
                        HStack(spacing: 8) {
                            Toggle("Band \(index + 1) enabled", isOn: Binding(
                                get: { model.settings.bands.indices.contains(index) && model.settings.bands[index].enabled },
                                set: { enabled in model.editBand(index) { $0.enabled = enabled } }))
                                .labelsHidden()
                                .accessibilityLabel("Band \(index + 1) enabled")
                                .accessibilityIdentifier("equalizer-band-row-\(index)-enabled")
                            Button {
                                model.selectedBand = index
                            } label: {
                                HStack {
                                    Text("\(index + 1)").frame(width: 24, alignment: .trailing)
                                    Text(band.kind.label).frame(width: 90, alignment: .leading)
                                    Text("\(band.frequency_hz.formatted(.number.precision(.fractionLength(0...2)))) Hz")
                                        .frame(width: 115, alignment: .trailing)
                                    Text(band.kind.usesGain ? String(format: "%+.2f dB", band.gain_db) : "—")
                                        .frame(width: 85, alignment: .trailing)
                                    Text(band.kind.isShelf
                                         ? String(format: "S %.3f", band.shelf_slope)
                                         : String(format: "Q %.4f", band.q))
                                    Spacer()
                                    if band.kind.isPass { Text("\(band.slope_db) dB/oct") }
                                    if !band.enabled { Text("Bypassed").foregroundStyle(.secondary) }
                                }
                                .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .accessibilityIdentifier("equalizer-band-row-\(index)")
                            .accessibilityAddTraits(model.selectedBand == index ? [.isSelected] : [])
                        }
                        .font(.callout).monospacedDigit()
                        .padding(.horizontal, 8).padding(.vertical, 5)
                        .background(model.selectedBand == index ? Color.accentColor.opacity(0.15) : Color.clear)
                        .clipShape(RoundedRectangle(cornerRadius: 5))
                        .id(index)
                    }
                }
            }
            .frame(height: min(CGFloat(model.settings.bands.count) * 34, 136))
            .accessibilityIdentifier("equalizer-band-list")
            .onChange(of: model.selectedBand) { _, index in scroll.scrollTo(index, anchor: .center) }
        }
    }

    private var bandEditor: some View {
        let index = model.selectedBand
        let bands = model.settings.bands
        let band = bands[index]
        return VStack(alignment: .leading, spacing: 10) {
            HStack {
                Picker("Selected band", selection: $model.selectedBand) {
                    ForEach(bands.indices, id: \.self) { i in
                        Text("\(i + 1): \(bands[i].kind.label)").tag(i)
                    }
                }.frame(width: 230).accessibilityIdentifier("equalizer-selected-band")
                Toggle("Band enabled", isOn: Binding(get: { band.enabled }, set: { enabled in
                    model.editBand(index) { $0.enabled = enabled }
                })).accessibilityIdentifier("equalizer-band-enabled")
                Spacer()
                Picker("Type", selection: Binding(get: { band.kind }, set: { kind in
                    model.editBand(index) { $0.kind = kind }
                })) {
                    ForEach(FilterKind.allCases, id: \.self) { Text($0.label).tag($0) }
                }.frame(width: 220).accessibilityIdentifier("equalizer-band-kind")
            }
            HStack(alignment: .top, spacing: 18) {
                number("Frequency Hz", "frequency", 20...20000, \.frequency_hz, step: 1)
                if band.kind.usesGain { number("Gain dB", "gain", -24...24, \.gain_db) }
                if band.kind.isShelf {
                    number("Shelf slope S", "shelf-slope", 0.1...1, \.shelf_slope, step: 0.01)
                } else {
                    number("Q", "q", 0.1...18, \.q, step: 0.01)
                }
                if band.kind.isPass {
                    Picker("Slope", selection: Binding(get: { band.slope_db }, set: { slope in
                        model.editBand(index) { $0.slope_db = slope }
                    })) {
                        ForEach([UInt8(12), 24, 36, 48], id: \.self) { Text("\($0) dB/oct").tag($0) }
                    }.frame(width: 160).accessibilityIdentifier("equalizer-band-slope")
                }
            }
            if band.kind.isPass {
                Text("Q scales only the highest-resonance section relative to Butterworth (0.7071). For higher slopes it is not the total filter bandwidth.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    private func number(_ label: String, _ id: String, _ range: ClosedRange<Double>,
                        _ key: WritableKeyPath<EqualizerBand, Double>, step: Double = 0.1) -> some View {
        let index = model.selectedBand
        return EqualizerNumberField(label: label, identifier: "equalizer-band-" + id, range: range, step: step,
                                    value: Binding(get: { model.settings.bands.indices.contains(index)
                                        ? model.settings.bands[index][keyPath: key] : range.lowerBound },
                                                   set: { value in model.editBand(index) { $0[keyPath: key] = value } }))
            .id("\(index)-\(id)")
    }

    private var headroom: some View {
        let peak = model.settings.peakGainDB
        let remaining = peak + model.settings.preamp_db
        return VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(String(format: "Estimated peak: %+.2f dB with preamp", remaining))
                    .foregroundStyle(remaining > 0.01 ? colours.error : Color.secondary)
                    .accessibilityIdentifier("equalizer-headroom")
                Spacer()
                Button("Compensate Preamp") { model.setPreamp(max(-96, min(0, -peak))) }
                    .accessibilityIdentifier("equalizer-compensate")
            }
            Text("Steady-state estimate over 20 Hz–20 kHz, including refined narrow peaks. Not a transient or inter-sample clipping guarantee; leave extra margin when needed.")
                .font(.caption).foregroundStyle(.secondary)
            if peak > 96 {
                Text("The −96 dB preamp limit cannot compensate this curve. Reduce band gains or resonance.")
                    .foregroundStyle(colours.error).accessibilityIdentifier("equalizer-headroom-insufficient")
            }
        }
    }

    private var presetEditor: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Picker("Saved preset", selection: $selectedPreset) {
                    Text("Choose…").tag(Optional<Data>.none)
                    ForEach(model.userPresets) { Text($0.name).tag(Optional($0.id)) }
                }.accessibilityIdentifier("equalizer-user-preset")
                Button("Recall") {
                    if let preset = model.userPresets.first(where: { $0.id == selectedPreset }) { model.recall(preset) }
                }.disabled(!model.userPresets.contains { $0.id == selectedPreset })
                    .accessibilityIdentifier("equalizer-preset-recall")
                Button("Delete") {
                    if let preset = model.userPresets.first(where: { $0.id == selectedPreset }) {
                        model.removePreset(name: preset.name)
                    }
                    selectedPreset = nil
                }
                    .disabled(!model.userPresets.contains { $0.id == selectedPreset })
                    .accessibilityIdentifier("equalizer-preset-delete")
            }
            HStack {
                TextField("Preset name", text: $presetName).textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier("equalizer-preset-name")
                Button(model.userPresets.contains { $0.matchesName(presetName) } ? "Overwrite Saved" : "Save Current") {
                    model.savePreset(name: presetName)
                }.disabled(!EqualizerUserPreset.isValidName(presetName))
                    .accessibilityIdentifier("equalizer-preset-save")
                Button("Rename Selected") {
                    if let preset = model.userPresets.first(where: { $0.id == selectedPreset }) {
                        model.renamePreset(name: preset.name, newName: presetName)
                    }
                    selectedPreset = nil
                }.disabled(!EqualizerUserPreset.isValidName(presetName) || !model.userPresets.contains { $0.id == selectedPreset })
                    .accessibilityIdentifier("equalizer-preset-rename")
            }
            Text("Saved on the server (up to 64). Save replaces an exact matching name. Recall preserves global bypass; saving, renaming and deleting do not alter the active curve.")
                .font(.caption).foregroundStyle(.secondary)
        }
        .disabled(model.presetBusy)
    }
}

private struct EqualizerNumberField: View {
    let label: String
    let identifier: String
    let range: ClosedRange<Double>
    var step: Double = 0.1
    @Binding var value: Double
    @State private var text = ""
    @State private var invalid = false
    @State private var synchronizedText = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 4) {
                Text(label).font(.caption)
                TextField(label, text: $text)
                    .textFieldStyle(.roundedBorder).monospacedDigit().frame(minWidth: 65)
                    .accessibilityIdentifier(identifier)
                    .onSubmit(commit)
                    .help("Enter a value and press Return to apply. Use the stepper for incremental changes.")
                Stepper(label, value: $value, in: range, step: step).labelsHidden()
                    .accessibilityIdentifier(identifier + "-step")
            }
            if invalid { Text("Enter \(range.lowerBound.formatted())…\(range.upperBound.formatted())").font(.caption).foregroundStyle(.red) }
        }
        .onAppear { synchronize() }
        .onChange(of: value) { _, _ in synchronize() }
    }
    private func synchronize() {
        text = value.formatted(.number.grouping(.never).precision(.fractionLength(0...4)))
        synchronizedText = text
        invalid = false
    }
    private func commit() {
        // Return without an actual text edit must not quantize a retained factory Q or dragged frequency.
        guard text != synchronizedText else { invalid = false; return }
        let normalized = text.trimmingCharacters(in: .whitespacesAndNewlines)
            .replacingOccurrences(of: Locale.current.decimalSeparator ?? ".", with: ".")
        guard let number = Double(normalized), number.isFinite, range.contains(number) else {
            invalid = true; return
        }
        value = number
        synchronize()
    }
}

private struct EqualizerResponseGraph: View {
    let model: EqualizerModel
    @State private var draggingBand: Int?

    var body: some View {
        let settings = model.settings
        let plot = settings.responsePlot(selectedBand: model.selectedBand)
        let frequencies = plot.frequencies
        let combined = plot.combinedDB
        let single = plot.selectedDB
        return GeometryReader { geometry in
            Canvas { context, size in
                func x(_ f: Double) -> Double { log(f / 20) / log(1000) * size.width }
                func y(_ gain: Double) -> Double { (24 - min(24, max(-48, gain))) / 72 * size.height }
                for gain in [-48.0, -24, 0, 24] {
                    var path = Path(); path.move(to: CGPoint(x: 0, y: y(gain))); path.addLine(to: CGPoint(x: size.width, y: y(gain)))
                    context.stroke(path, with: .color(.secondary.opacity(gain == 0 ? 0.5 : 0.2)))
                    context.draw(Text("\(Int(gain)) dB").font(.caption2).foregroundStyle(.secondary), at: CGPoint(x: 4, y: max(8, min(size.height - 25, y(gain)))), anchor: .leading)
                }
                for frequency in [20.0, 100, 1000, 10000, 20000] {
                    var path = Path(); path.move(to: CGPoint(x: x(frequency), y: 0)); path.addLine(to: CGPoint(x: x(frequency), y: size.height))
                    context.stroke(path, with: .color(.secondary.opacity(0.2)))
                }
                func curve(_ gains: [Double], _ colour: Color, _ dashed: Bool) {
                    var path = Path()
                    for i in frequencies.indices {
                        let point = CGPoint(x: x(frequencies[i]), y: y(gains[i]))
                        if i == 0 { path.move(to: point) } else { path.addLine(to: point) }
                    }
                    context.stroke(path, with: .color(colour), style: StrokeStyle(lineWidth: 2, dash: dashed ? [5, 3] : []))
                }
                curve(combined, .accentColor, false)
                if let single { curve(single, .orange, true) }
                for i in settings.bands.indices {
                    let band = settings.bands[i]
                    let point = CGPoint(x: x(band.frequency_hz), y: y(band.kind.usesGain ? band.gain_db : 0))
                    let circle = Path(ellipseIn: CGRect(x: point.x - 6, y: point.y - 6, width: 12, height: 12))
                    context.fill(circle, with: .color(i == model.selectedBand ? .orange : .accentColor.opacity(band.enabled ? 1 : 0.35)))
                }
                for frequency in [20.0, 100, 1000, 10000, 20000] {
                    let label = frequency < 1000 ? "\(Int(frequency)) Hz" : "\(Int(frequency / 1000)) kHz"
                    let anchor: UnitPoint = frequency == 20 ? .leading : frequency == 20000 ? .trailing : .center
                    context.draw(Text(label).font(.caption2).foregroundStyle(.secondary),
                                 at: CGPoint(x: min(size.width - 4, max(4, x(frequency))), y: size.height - 8),
                                 anchor: anchor)
                }
            }
            .background(.quaternary.opacity(0.3))
            .contentShape(Rectangle())
            .gesture(DragGesture(minimumDistance: 0).onChanged { drag in
                guard !settings.bands.isEmpty else { return }
                if draggingBand == nil {
                    draggingBand = settings.bands.indices.min { a, b in
                        func distance(_ i: Int) -> Double {
                            let band = settings.bands[i]
                            let x = log(band.frequency_hz / 20) / log(1000) * geometry.size.width
                            let y = (24 - (band.kind.usesGain ? band.gain_db : 0)) / 72 * geometry.size.height
                            return pow(x - drag.startLocation.x, 2) + pow(y - drag.startLocation.y, 2)
                        }
                        return distance(a) < distance(b)
                    }
                }
                guard let index = draggingBand else { return }
                model.selectedBand = index
                let frequency = 20 * pow(1000, min(1, max(0, drag.location.x / geometry.size.width)))
                let gain = min(24, max(-24, 24 - drag.location.y / geometry.size.height * 72))
                model.editBand(index) { band in
                    band.frequency_hz = frequency
                    if band.kind.usesGain { band.gain_db = gain }
                }
            }.onEnded { _ in draggingBand = nil })
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("Equalizer response graph. Solid combined curve including preamp; dashed selected band. Drag a band to change frequency and gain. Numeric controls provide keyboard and VoiceOver editing.")
            .accessibilityIdentifier("equalizer-response-graph")
        }
    }
}

/// Retain the transport slider's native AX action path and half-decibel VoiceOver steps.
private struct EqualizerGainSlider: NSViewRepresentable {
    @Binding var value: Double
    let range: ClosedRange<Double>
    let enabled: Bool
    let identifier: String
    let label: String
    func makeCoordinator() -> Coordinator { Coordinator(value: $value) }
    func makeNSView(context: Context) -> AccessibleSlider {
        let slider = AccessibleSlider()
        slider.minValue = range.lowerBound; slider.maxValue = range.upperBound
        slider.isContinuous = true; slider.accessibilityStep = 0.5
        slider.target = context.coordinator; slider.action = #selector(Coordinator.changed(_:))
        slider.setAccessibilityIdentifier(identifier); slider.setAccessibilityLabel(label); slider.toolTip = label
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
        @objc func changed(_ slider: NSSlider) { value.wrappedValue = (slider.doubleValue * 2).rounded() / 2 }
    }
}
