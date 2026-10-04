// Sample compiled module in C#, published with NativeAOT: a counter changed by buttons, a slider
// and a spin box, each change one undo entry, with its history painted as bars; a curve edited in
// the curve editor of the module curves, each change one undo entry; the counter's value as an
// animatable property; and two commands.

using System.Runtime.InteropServices;
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

    const string StartCurve =
        """[{"label":"easing","colour":[230,160,40],"keys":[{"time":0,"value":0},{"time":1,"value":1}]}]""";

    static readonly Command[] Commands =
    [
        new("cs.sum", "The sum of numbers, computed in C#.",
            """{"type":"object","properties":{"numbers":{"type":"array","items":{"type":"number"}}},"required":["numbers"]}""",
            """{"type":"object","properties":{"sum":{"type":"number"}}}""", Sum),
        new("cs.paint_from_threads",
            "Four threads paint the cube twice each; the two paints of each thread are one undo entry.",
            """{"type":"object"}""", """{"type":"object","properties":{"threads":{"type":"integer"}}}""",
            PaintFromThreads),
    ];

    static readonly PanelDeclaration[] Panels = [new("counter", "Counter", Area.Right)];

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
            Editor.DeclareProperty("value", "Value", ValueKind.Number, Minimum, Maximum, [counter], WriteValue);
            Editor.Describe(info, "sample-csharp", "0.1.0", Commands, Panels, ApplyChange);
            described = true;
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
        layout.AddWidget(new Separator());
        layout.AddLayout(trials);
        new Panel("counter").SetLayout(layout);
        Show();
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
}
