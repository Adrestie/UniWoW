// Sample compiled module in C#, published with NativeAOT: a counter changed by buttons, a slider
// and a spin box, each change one undo entry, with its history painted as bars; a curve edited in
// the curve editor of the module curves, each change one undo entry; the counter's value as an
// animatable property, with a sequence of its own shown in a dopesheet, whose changes the editor
// records; a table of 100,000 rows, a tree and a property grid of the cube; two commands; and a
// component of the scene on the doodads, a tag and a weight, declared through scene.declare, read and
// written through its commands, each change one undo entry.

using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using UniWoW;
using static System.FormattableString;

namespace SampleCsharp;

static unsafe class Module
{
    const int Minimum = 0;
    const int Maximum = 100;
    const int ShownBars = 40;
    const int Threads = 4;

    // Touched on the module's thread only, by the slots and by ApplyChange.
    static int counter;
    // The value of the last change recorded: while the slider or the spin box is dragged, only
    // `counter` follows it.
    static int committed;
    // The values the counter took, one per change.
    static readonly List<int> history = [];
    static bool failToApply;
    static bool failToWrite;
    // Set once the module is described: its property can be told from then on.
    static bool described;

    static Label shown = null!;
    static Slider slider = null!;
    static SpinBox spin = null!;
    static PaintArea bars = null!;
    static CurveView curve = null!;
    // The curve as last recorded: what the next change of it undoes to.
    static string committedCurve = "";

    // The rows the table of the panel Data starts with.
    const int TableRows = 100_000;
    // The id the next row inserted takes.
    static ulong nextRow = TableRows + 1;
    static TableView table = null!;
    static Label dataEvent = null!;

    const string Zones =
        """[{"id":1,"text":"Kalimdor","expanded":true,"children":[{"id":2,"text":"Durotar"},{"id":3,"text":"Mulgore"}]},{"id":4,"text":"Eastern Kingdoms","children":[{"id":5,"text":"Elwynn Forest"},{"id":6,"text":"Westfall","children":[{"id":7,"text":"Sentinel Hill"}]}]}]""";

    const string StartCurve =
        """[{"label":"easing","colour":[230,160,40],"keys":[{"time":0,"value":0},{"time":1,"value":1}]}]""";

    // The keys of the counter's own sequence, over 2 seconds at 30 frames per second.
    const string ValueTrack =
        """[{"property":"sample-csharp/value","kind":"number","curves":[{"keys":[{"time":0,"value":0},{"time":30,"value":100},{"time":60,"value":50}]}]}]""";

    static readonly Command[] Commands =
    [
        new("cs.sum", "The sum of numbers, computed in C#.",
            """{"type":"object","properties":{"numbers":{"type":"array","items":{"type":"number"}}},"required":["numbers"]}""",
            """{"type":"object","properties":{"sum":{"type":"number"}}}""", Sum),
        new("cs.paint_from_threads",
            "Four threads paint the cube twice each; the two paints of each thread are one undo entry.",
            """{"type":"object"}""", """{"type":"object","properties":{"threads":{"type":"integer"}}}""",
            PaintFromThreads),
        new("cs.tag_read", "The tags and weights of the doodads 'ids', the component Tag (C#).",
            """{"type":"object","properties":{"ids":{"type":"array"}},"required":["ids"]}""",
            """{"type":"object","properties":{"entities":{"type":"array"}}}""", ReadTags),
        new("cs.tag_write", "Changes the 'field' (tag, weight) of the doodads 'ids' to 'change'; one undo entry.",
            """{"type":"object","properties":{"ids":{"type":"array"},"field":{"type":"string"},"change":{"type":"string"}},"required":["ids","field","change"]}""",
            """{"type":"object","properties":{"changed":{"type":"integer"},"refused":{"type":"array"}}}""", WriteTags),
    ];

    // The component Tag (C#): a tag and a weight by doodad, read and written on the callers'
    // threads, applied again by ApplyChange on the module's.
    static readonly Dictionary<string, (string Tag, double Weight)> tags = [];
    static readonly object tagsLock = new();

    // The drawing of its own the Inspector shows under the fields of Tag (C#): the weights of the
    // doodads last read as bars.
    static VBoxLayout tagLayout = null!;
    static PaintArea tagBars = null!;
    static double[] shownWeights = [];

    static string TagDeclaration() =>
        """{"components":[{"name":"cs.tag","label":"Tag (C#)","kinds":["doodad"],"fields":[{"name":"tag","label":"Tag","kind":"text"},{"name":"weight","label":"Weight","kind":"number"}],"read":"cs.tag_read","write":"cs.tag_write","layout":"""
        + Invariant($"{tagLayout.Handle}") + "}]}";

    static readonly PanelDeclaration[] Panels =
        [new("counter", "Counter", Area.Right), new("data", "Data", Area.Right)];

    [UnmanagedCallersOnly(EntryPoint = "uniwow_module_init")]
    static int Init(Api* api, ModuleInfo* info, delegate* unmanaged<IntPtr, byte*, void> error, IntPtr errorContext)
    {
        try
        {
            if (api->Version != Editor.ApiVersion)
            {
                throw new InvalidOperationException("built for another version of uniwow.h");
            }
            Editor.Start(api);
            BuildPanel();
            BuildData();
            BuildTag();
            Editor.DeclareProperty("value", "Value", ValueKind.Number, Minimum, Maximum, [counter], WriteValue);
            Editor.Describe(info, "sample-csharp", "0.1.0", Commands, Panels, ApplyChange);
            described = true;
            new Thread(DeclareTag) { IsBackground = true }.Start();
            return 0;
        }
        catch (Exception failure)
        {
            Editor.Reply(error, errorContext, failure.Message);
            return 1;
        }
    }

    static void BuildPanel()
    {
        shown = new Label();
        var buttons = new HBoxLayout();
        foreach (int delta in new[] { -10, -1, 1, 10 })
        {
            var button = new PushButton(Invariant($"{delta:+0;-0}"));
            button.Clicked.Connect(() => Step(delta));
            buttons.AddWidget(button);
        }
        var reset = new PushButton("Reset");
        reset.Clicked.Connect(() =>
        {
            counter = 0;
            Commit("reset the counter");
        });
        buttons.AddWidget(reset);

        slider = new Slider();
        slider.SetRange(Minimum, Maximum);
        slider.SetSingleStep(1);
        slider.ValueChanged.Connect(value =>
        {
            counter = (int)Math.Round(value);
            spin.SetValue(counter);
            ShowLive();
        });
        slider.SliderReleased.Connect(_ => Commit("set the counter with the slider"));

        spin = new SpinBox();
        spin.SetRange(Minimum, Maximum);
        spin.SetSingleStep(1);
        spin.SetDecimals(0);
        spin.ValueChanged.Connect(value =>
        {
            counter = (int)Math.Round(value);
            slider.SetValue(counter);
            ShowLive();
        });
        spin.EditingFinished.Connect(_ => Commit("set the counter with the spin box"));

        var values = new HBoxLayout();
        values.AddWidget(slider);
        values.AddWidget(spin);

        bars = new PaintArea();
        bars.SetMinimumHeight(140);
        bars.Paint(Paint);

        curve = new CurveView();
        curve.SetMinimumHeight(160);
        curve.SetCurves(StartCurve);
        committedCurve = curve.Curves();
        curve.CurvesChanged.Connect(change =>
        {
            if (!change.Finished || change.Json == committedCurve)
            {
                return;
            }
            Editor.RecordChange("edit the curve", Invariant($$"""{"curve":{{committedCurve}}}"""),
                                Invariant($$"""{"curve":{{change.Json}}}"""));
            committedCurve = change.Json;
        });

        // A dopesheet of the counter's sequence: the editor records each change of its keys, and
        // moving its playhead writes the value at that frame. The module records nothing.
        var sequence = new Sequence();
        sequence.SetLength(60);
        sequence.SetTracks(ValueTrack);
        var player = new Player();
        player.SetSequence(sequence);
        var keys = new DopesheetView();
        keys.SetSequence(sequence);
        keys.SetPlayer(player);
        keys.SetTitle("Counter");
        keys.SetMinimumHeight(110);

        var fail = new CheckBox("Fail to apply undo and redo");
        fail.SetToolTip("The module then fails at the next undo or redo of its changes.");
        fail.Toggled.Connect(on => failToApply = on);
        var failWrite = new CheckBox("Fail to write the value");
        failWrite.SetToolTip("The module then fails at the next write of its property, by the Timeline for one.");
        failWrite.Toggled.Connect(on => failToWrite = on);
        var wait = new PushButton("Wait 2 seconds");
        wait.SetToolTip("A slot taking two seconds: the signals that follow wait their turn.");
        wait.Clicked.Connect(() =>
        {
            shown.SetText("Waiting 2 seconds...");
            Thread.Sleep(2000);
            Show();
        });
        var trials = new HBoxLayout();
        trials.AddWidget(fail);
        trials.AddWidget(failWrite);
        trials.AddWidget(wait);

        var layout = new VBoxLayout();
        layout.AddWidget(shown);
        layout.AddLayout(buttons);
        layout.AddLayout(values);
        layout.AddWidget(bars);
        layout.AddWidget(curve);
        layout.AddWidget(keys);
        layout.AddWidget(new Separator());
        layout.AddLayout(trials);
        new Panel("counter").SetLayout(layout);
        Show();
    }

    // The panel Data: a table of 100,000 rows, edited in place and sorted by a column, rows
    // inserted and removed without giving the others; a tree; and the cube's properties, each value
    // changed by hand an undo entry the editor records. The last signal is shown above.
    static void BuildData()
    {
        dataEvent = new Label("Double-click a cell to edit it; click a header to sort by its column.");
        table = new TableView();
        table.SetMinimumHeight(260);
        table.SetColumns("""["Id","Name","Value"]""");
        var rows = new StringBuilder("[", TableRows * 48);
        for (int id = 1; id <= TableRows; id++)
        {
            rows.Append(id == 1 ? "" : ",")
                .Append(Invariant($$"""{"id":{{id}},"cells":["{{id}}","Item {{id}}","{{id * 7919 % 1000}}"]}"""));
        }
        table.SetRows(rows.Append(']').ToString());
        table.CellChanged.Connect(cell =>
            dataEvent.SetText(Invariant($"Row {cell.Row}, column {cell.Column} edited: \"{cell.Text}\"")));
        table.CurrentCellChanged.Connect(cell =>
            dataEvent.SetText(Invariant($"Current cell: row {cell.Row}, column {cell.Column}")));
        table.SortChanged.Connect(sort => dataEvent.SetText(
            Invariant($"Sorted by column {sort.Column}, {(sort.Descending ? "from the highest" : "from the lowest")}")));

        var insert = new PushButton("Insert a row at the top");
        insert.Clicked.Connect(() =>
        {
            ulong id = nextRow++;
            table.InsertRows(0, Invariant($$"""[{"id":{{id}},"cells":["{{id}}","New {{id}}","0"]}]"""));
            dataEvent.SetText(Invariant($"Row {id} inserted"));
        });
        var remove = new PushButton("Remove the current row");
        remove.Clicked.Connect(() =>
        {
            ulong id = table.CurrentRow();
            dataEvent.SetText(id == 0 ? "No current row."
                : table.RemoveRows(id) ? Invariant($"Row {id} removed")
                : Invariant($"Row {id} not removed"));
        });
        var buttons = new HBoxLayout();
        buttons.AddWidget(insert);
        buttons.AddWidget(remove);

        var tree = new TreeView();
        tree.SetMinimumHeight(140);
        tree.SetItems(Zones);
        tree.CurrentItemChanged.Connect(item => dataEvent.SetText(Invariant($"Current item: {item}")));
        tree.ItemExpanded.Connect(change =>
            dataEvent.SetText(Invariant($"Item {change.Item} {(change.Expanded ? "unfolded" : "folded")}")));

        var cube = new PropertyGrid();
        cube.SetMinimumHeight(110);
        cube.SetPaths("""["sample-cube/position","sample-cube/rotation","sample-cube/scale","sample-cube/colour"]""");

        var layout = new VBoxLayout();
        layout.AddWidget(dataEvent);
        layout.AddWidget(table);
        layout.AddLayout(buttons);
        layout.AddWidget(tree);
        layout.AddWidget(cube);
        new Panel("data").SetLayout(layout);
    }

    static void Step(int delta)
    {
        counter = Math.Clamp(counter + delta, Minimum, Maximum);
        Commit(Invariant($"counter {delta:+0;-0}"));
    }

    // Records the change from the committed value to the counter, as one undo entry.
    static void Commit(string label)
    {
        if (counter != committed)
        {
            int before = history.Count;
            history.Add(counter);
            Editor.RecordChange(label, Invariant($$"""{"value":{{committed}},"bars":{{before}}}"""),
                                Invariant($$"""{"value":{{counter}},"bars":{{before + 1}}}"""));
            committed = counter;
        }
        Show();
    }

    // Applies an undo or redo value: the counter, and how many bars the history has then.
    static void ApplyChange(string value)
    {
        if (failToApply)
        {
            throw new InvalidOperationException("asked to fail by its check box");
        }
        using var document = JsonDocument.Parse(value);
        if (document.RootElement.TryGetProperty("tags", out var recordedTags))
        {
            lock (tagsLock)
            {
                tags.Clear();
                foreach (var entry in recordedTags.EnumerateObject())
                {
                    tags[entry.Name] = (entry.Value.GetProperty("tag").GetString() ?? "",
                                        entry.Value.GetProperty("weight").GetDouble());
                }
            }
            return;
        }
        if (document.RootElement.TryGetProperty("curve", out var recorded))
        {
            committedCurve = recorded.GetRawText();
            curve.SetCurves(committedCurve);
            return;
        }
        int target = document.RootElement.GetProperty("value").GetInt32();
        int count = document.RootElement.GetProperty("bars").GetInt32();
        if (count == history.Count + 1)
        {
            history.Add(target);
        }
        else if (count <= history.Count)
        {
            history.RemoveRange(count, history.Count - count);
        }
        else
        {
            throw new InvalidOperationException("the history of the counter does not match the value");
        }
        counter = committed = target;
        Show();
    }

    // The value written to the property `value`, by the Timeline for one, without the history: a
    // whole number within the range, which the editor then shows.
    static void WriteValue(double[] value)
    {
        if (failToWrite)
        {
            throw new InvalidOperationException("asked to fail by its check box");
        }
        counter = Math.Clamp((int)Math.Round(value[0]), Minimum, Maximum);
        value[0] = counter;
        Show();
    }

    // The counter in every widget, and the bars painted again.
    static void Show()
    {
        slider.SetValue(counter);
        spin.SetValue(counter);
        ShowLive();
    }

    // The same, without touching the widget being dragged.
    static void ShowLive()
    {
        shown.SetText(Invariant($"Counter: {counter}"));
        bars.Update();
        if (described)
        {
            Editor.SetProperty("value", counter);
        }
    }

    static void Paint(Painter painter, double width, double height)
    {
        painter.SetPen(Colors.Rgba(70, 70, 70));
        painter.SetBrush(Colors.Rgba(28, 28, 28));
        painter.DrawRect(0, 0, width, height, 4);
        painter.SetPen(Colors.Rgba(200, 200, 200));
        int first = Math.Max(0, history.Count - ShownBars);
        painter.DrawText(8, 6, history.Count == 0
            ? "No change yet."
            : Invariant($"{history.Count} changes, the last {history.Count - first} shown"), 12);
        double top = 28, bottom = height - 8, step = (width - 16) / ShownBars;
        painter.SetPen(Colors.Rgba(0, 0, 0, 0), 0);
        for (int i = first; i < history.Count; i++)
        {
            double bar = Math.Max(1, (bottom - top) * history[i] / Maximum);
            painter.SetBrush(i == history.Count - 1 ? Colors.Rgba(255, 184, 46) : Colors.Rgba(70, 130, 200));
            painter.DrawRect(8 + (i - first) * step + 1, bottom - bar, Math.Max(1, step - 2), bar, 2);
        }
    }

    static string Sum(string arguments)
    {
        using var document = JsonDocument.Parse(arguments);
        if (!document.RootElement.TryGetProperty("numbers", out var numbers) || numbers.ValueKind != JsonValueKind.Array)
        {
            throw new ArgumentException("numbers must be an array of numbers");
        }
        double sum = 0;
        foreach (var number in numbers.EnumerateArray())
        {
            if (number.ValueKind != JsonValueKind.Number)
            {
                throw new ArgumentException("numbers must be an array of numbers");
            }
            sum += number.GetDouble();
        }
        if (!double.IsFinite(sum))
        {
            throw new ArgumentException("the sum is too large");
        }
        return Invariant($$"""{"sum":{{sum}}}""");
    }

    static string PaintFromThreads(string arguments)
    {
        for (int i = 0; i < Threads; i++)
        {
            int index = i;
            new Thread(() => PaintTwice(index)) { IsBackground = true }.Start();
        }
        return Invariant($$"""{"threads":{{Threads}}}""");
    }

    // Two paints of the cube from one thread, as one undo entry of their own.
    static void PaintTwice(int index)
    {
        try
        {
            Editor.BeginGroup(Invariant($"C# thread {index + 1}"));
            var first = Editor.Call("cube.paint", Invariant($$"""{"color":[{{0.2 * index}},0.5,0.9]}"""));
            var second = Editor.Call("cube.paint", Invariant($$"""{"color":[0.9,{{0.2 * index}},0.3]}"""));
            Editor.EndGroup();
            Editor.Log(LogLevel.Information, Invariant(
                $"C# thread {index + 1} on thread {Environment.CurrentManagedThreadId}: {first.Answer}, {second.Answer}"));
        }
        catch (Exception failure)
        {
            Editor.Log(LogLevel.Error, Invariant($"C# thread {index + 1}: {failure.Message}"));
        }
    }

    // Declares the component Tag (C#) once the scene answers: it may start after this module.
    static void DeclareTag()
    {
        for (int attempt = 0; attempt < 40; attempt++)
        {
            var (ok, answer) = Editor.Call("scene.declare", TagDeclaration());
            if (ok)
            {
                return;
            }
            if (attempt == 39)
            {
                Editor.Log(LogLevel.Warning, "the component Tag (C#) is not declared: " + answer);
            }
            Thread.Sleep(500);
        }
    }

    static void BuildTag()
    {
        tagLayout = new VBoxLayout();
        tagBars = new PaintArea();
        tagBars.SetMinimumHeight(48);
        tagBars.Paint(PaintWeights);
        tagLayout.AddWidget(tagBars);
    }

    static void PaintWeights(Painter painter, double width, double height)
    {
        double[] weights;
        lock (tagsLock)
        {
            weights = shownWeights;
        }
        painter.SetPen(Colors.Rgba(70, 70, 70));
        painter.SetBrush(Colors.Rgba(28, 28, 28));
        painter.DrawRect(0, 0, width, height, 4);
        double highest = Math.Max(1, weights.DefaultIfEmpty(0).Max(Math.Abs));
        double step = (width - 16) / Math.Max(1, weights.Length);
        painter.SetPen(Colors.Rgba(0, 0, 0, 0), 0);
        painter.SetBrush(Colors.Rgba(255, 184, 46));
        for (int i = 0; i < weights.Length; i++)
        {
            double bar = Math.Max(1, (height - 12) * Math.Abs(weights[i]) / highest);
            painter.DrawRect(8 + i * step + 1, height - 6 - bar, Math.Max(1, step - 2), bar, 2);
        }
    }

    static List<string> Ids(JsonElement root)
    {
        if (!root.TryGetProperty("ids", out var ids) || ids.ValueKind != JsonValueKind.Array)
        {
            throw new ArgumentException("'ids' is a list of ids");
        }
        return [.. ids.EnumerateArray().Select(id => id.GetString() ?? throw new ArgumentException("an id is text"))];
    }

    static string Text(string text) => JsonEncodedText.Encode(text).ToString();

    static string Fields(string tag, double weight) =>
        "{\"tag\":\"" + Text(tag) + "\",\"weight\":" + Invariant($"{weight}") + "}";

    // The tags as JSON, for an undo entry; the lock is held.
    static string TagsJson()
    {
        var entries = tags.OrderBy(entry => entry.Key, StringComparer.Ordinal)
            .Select(entry => "\"" + Text(entry.Key) + "\":" + Fields(entry.Value.Tag, entry.Value.Weight));
        return "{\"tags\":{" + string.Join(",", entries) + "}}";
    }

    static string ReadTags(string arguments)
    {
        using var document = JsonDocument.Parse(arguments);
        var ids = Ids(document.RootElement);
        lock (tagsLock)
        {
            var entities = ids.Select(id =>
            {
                var (tag, weight) = tags.TryGetValue(id, out var held) ? held : ("", 0.0);
                return "{\"id\":\"" + Text(id) + "\",\"fields\":" + Fields(tag, weight) + "}";
            });
            shownWeights = [.. ids.Select(id => tags.TryGetValue(id, out var held) ? held.Weight : 0.0)];
            string answer = "{\"entities\":[" + string.Join(",", entities) + "]}";
            tagBars.Update();
            return answer;
        }
    }

    // A number changed as the Inspector types it: a value, or +n, -=n, *n, /n.
    static double Changed(double value, string change)
    {
        static double Number(string text) =>
            double.TryParse(text, System.Globalization.NumberStyles.Float, System.Globalization.CultureInfo.InvariantCulture, out var number) && double.IsFinite(number)
                ? number
                : throw new ArgumentException($"'{text}' is not a number");
        change = change.Trim();
        double result = change switch
        {
            _ when change.StartsWith("-=") => value - Number(change[2..]),
            _ when change.StartsWith('+') => value + Number(change[1..]),
            _ when change.StartsWith('*') => value * Number(change[1..]),
            _ when change.StartsWith('/') => value / Number(change[1..]),
            _ => Number(change),
        };
        return double.IsFinite(result) ? result : throw new ArgumentException("the weight is not finite");
    }

    static string WriteTags(string arguments)
    {
        using var document = JsonDocument.Parse(arguments);
        var root = document.RootElement;
        var ids = Ids(root);
        string field = root.GetProperty("field").GetString() ?? "";
        string change = root.GetProperty("change").GetString() ?? "";
        lock (tagsLock)
        {
            string before = TagsJson();
            foreach (var id in ids)
            {
                var (tag, weight) = tags.TryGetValue(id, out var held) ? held : ("", 0.0);
                switch (field)
                {
                    case "tag": tag = change; break;
                    case "weight": weight = Changed(weight, change); break;
                    default: throw new ArgumentException($"the tag has no field '{field}'");
                }
                if (tag.Length == 0 && weight == 0)
                {
                    tags.Remove(id);
                }
                else
                {
                    tags[id] = (tag, weight);
                }
            }
            string after = TagsJson();
            if (before != after)
            {
                Editor.RecordChange("set the " + field + " (C#)", before, after);
            }
        }
        return Invariant($$"""{"changed":{{ids.Count}},"refused":[]}""");
    }
}
