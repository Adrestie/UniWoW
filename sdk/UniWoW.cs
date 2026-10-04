// UniWoW for C#: classes named as in Qt over the C interface of uniwow.h, for a module compiled
// with NativeAOT. Add this file to the module's project, which allows unsafe code.
//
// Call Editor.Start(api) first in the module's entry point, and Editor.Describe(info, ...) last.
// Objects are handles: Destroy() destroys the object. Signals connect to functions, as in Qt for
// Python: button.Clicked.Connect(() => ...). Slots run on the module's own thread, never on the
// editor's interface thread; an exception a slot lets out is logged. Disconnect, and Destroy on
// the sender or one of its parents, let the connected function go. A command handler or the
// function applying undo and redo values reports a failure by throwing.

using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;

namespace UniWoW;

/// <summary>The table of uniwow.h, field for field.</summary>
[StructLayout(LayoutKind.Sequential)]
public unsafe struct Api
{
    public uint Version;
    public IntPtr Context;
    public delegate* unmanaged<IntPtr, delegate* unmanaged<IntPtr, byte*, void>, IntPtr, void> Commands;
    public delegate* unmanaged<IntPtr, byte*, byte*, delegate* unmanaged<IntPtr, byte*, void>, IntPtr, int> Call;
    public delegate* unmanaged<IntPtr, byte*, byte*, void> Publish;
    public delegate* unmanaged<IntPtr, byte*, ulong> Subscribe;
    public delegate* unmanaged<IntPtr, ulong, uint, delegate* unmanaged<IntPtr, byte*, void>, IntPtr, int> NextEvent;
    public delegate* unmanaged<IntPtr, ulong, void> Unsubscribe;
    public delegate* unmanaged<IntPtr, byte*, delegate* unmanaged<IntPtr, byte*, void>, IntPtr, void> Setting;
    public delegate* unmanaged<IntPtr, byte*, byte*, void> SetSetting;
    public delegate* unmanaged<IntPtr, int, byte*, void> Log;
    public delegate* unmanaged<IntPtr, byte*, void> BeginGroup;
    public delegate* unmanaged<IntPtr, void> EndGroup;
    public delegate* unmanaged<IntPtr, byte*, byte*, byte*, int> RecordChange;

    public delegate* unmanaged<IntPtr, byte*, ulong> Panel;
    public delegate* unmanaged<IntPtr, uint, ulong, ulong> Create;
    public delegate* unmanaged<IntPtr, ulong, void> Destroy;
    public delegate* unmanaged<IntPtr, ulong, ulong, uint, uint, uint, uint, int> AddTo;
    public delegate* unmanaged<IntPtr, ulong, uint, byte*, int> SetText;
    public delegate* unmanaged<IntPtr, ulong, uint, double*, uint, int> SetNumbers;
    public delegate* unmanaged<IntPtr, ulong, uint, delegate* unmanaged<IntPtr, byte*, void>, IntPtr, int> Text;
    public delegate* unmanaged<IntPtr, ulong, uint, double*, uint, uint> Numbers;
    public delegate* unmanaged<IntPtr, ulong, byte*, int> AddEntry;
    public delegate* unmanaged<IntPtr, ulong, int> ClearEntries;
    public delegate* unmanaged<IntPtr, ulong, ulong, int> SetScene;
    public delegate* unmanaged<IntPtr, ulong, int> Update;
    public delegate* unmanaged<IntPtr, ulong, uint, delegate* unmanaged<IntPtr, SignalData*, void>, IntPtr, ulong> Connect;
    public delegate* unmanaged<IntPtr, ulong, void> Disconnect;

    public delegate* unmanaged<IntPtr, ulong, uint, double, void> SetPen;
    public delegate* unmanaged<IntPtr, ulong, uint, void> SetBrush;
    public delegate* unmanaged<IntPtr, ulong, double, double, double, double, void> DrawLine;
    public delegate* unmanaged<IntPtr, ulong, double, double, double, double, double, void> DrawRect;
    public delegate* unmanaged<IntPtr, ulong, double, double, double, double, void> DrawEllipse;
    public delegate* unmanaged<IntPtr, ulong, double, double, byte*, double, void> DrawText;
    public delegate* unmanaged<IntPtr, ulong, double, double, void> Translate;
    public delegate* unmanaged<IntPtr, ulong, double, double, void> Scale;
    public delegate* unmanaged<IntPtr, ulong, void> Save;
    public delegate* unmanaged<IntPtr, ulong, void> Restore;
}

/// <summary>uniwow_signal: what a slot receives; the fields its signal does not use are zero.</summary>
[StructLayout(LayoutKind.Sequential)]
public unsafe struct SignalData
{
    public ulong Sender;
    public uint Signal;
    public uint Button;
    public uint Modifiers;
    public int Boolean;
    public ulong Item;
    public long Integer;
    public double Number;
    public byte* TextPointer;
    public double X, Y, Dx, Dy, Width, Height;
    public ulong Painter;

    /// <summary>The text of the signal, valid during the slot only, hence copied here.</summary>
    public readonly string Text => Utf8.Read(TextPointer);
}

/// <summary>uniwow_command.</summary>
[StructLayout(LayoutKind.Sequential)]
public unsafe struct NativeCommand
{
    public byte* Name, Description, ArgumentsSchema, ResultSchema;
    public delegate* unmanaged<IntPtr, byte*, delegate* unmanaged<IntPtr, byte*, void>, IntPtr, int> Handler;
    public IntPtr User;
}

/// <summary>uniwow_panel.</summary>
[StructLayout(LayoutKind.Sequential)]
public unsafe struct NativePanel
{
    public byte* Id, Title;
    public uint Area;
}

/// <summary>uniwow_module_info.</summary>
[StructLayout(LayoutKind.Sequential)]
public unsafe struct ModuleInfo
{
    public byte* Name, Version;
    public NativeCommand* Commands;
    public uint CommandCount, HeaderVersion, CommandSize, PanelCount;
    public NativePanel* Panels;
    public delegate* unmanaged<IntPtr, byte*, delegate* unmanaged<IntPtr, byte*, void>, IntPtr, int> ApplyChange;
    public IntPtr User;
}

public enum Kind : uint
{
    Panel = 1, Label, PushButton, CheckBox, Slider, SpinBox, LineEdit, ComboBox, Separator, GroupBox,
    VBoxLayout, HBoxLayout, GridLayout, GraphicsView, GraphicsScene, RectItem, LineItem, EllipseItem,
    TextItem, ItemGroup, PaintArea, Dialog, CurveView,
}

public enum Property : uint
{
    Text = 1, ToolTip, Enabled, Visible, Checked, Value, Minimum, Maximum, Step, Decimals, Placeholder,
    CurrentIndex, Title, Pos, Rect, Line, PenColor, PenWidth, BrushColor, Radius, ZValue, Movable,
    Selectable, Selected, MoveBounds, FontSize, MinimumHeight, ViewScale, ViewCenter, Count, Curves,
}

public enum SignalId : uint
{
    Clicked = 1, Toggled, ValueChanged, SliderPressed, SliderReleased, TextChanged, EditingFinished,
    CurrentIndexChanged, ItemPressed, ItemMoved, ItemDoubleClicked, SelectionChanged, Paint, MousePress,
    MouseMove, MouseRelease, Wheel, Rejected, CurvesChanged,
}

public enum LogLevel { Error = 1, Warning = 2, Information = 3, Debug = 4 }

/// <summary>Where a panel docks first.</summary>
public enum Area : uint { Centre = 1, Left = 2, Right = 3, Bottom = 4 }

/// <summary>A command the module offers, run on the calling thread, possibly several at once:
/// <paramref name="Handler"/> receives the JSON arguments and returns the JSON result, or throws.
/// The schemas are JSON Schema.</summary>
public sealed record Command(string Name, string Description, string ArgumentsSchema, string ResultSchema,
                             Func<string, string> Handler);

/// <summary>A panel of the module, holding one layout set with Panel.SetLayout.</summary>
public sealed record PanelDeclaration(string Id, string Title, Area Area);

/// <summary>A UTF-8 copy of a text for the C interface, freed when disposed.</summary>
public readonly unsafe struct Utf8 : IDisposable
{
    public readonly byte* Pointer;

    public Utf8(string text) => Pointer = Keep(text);

    public void Dispose() => Marshal.FreeCoTaskMem((IntPtr)Pointer);

    /// <summary>A copy never freed, for the texts that must stay valid while the module is loaded.</summary>
    public static byte* Keep(string text) => (byte*)Marshal.StringToCoTaskMemUTF8(text);

    public static string Read(byte* text) => text == null ? "" : Marshal.PtrToStringUTF8((IntPtr)text) ?? "";
}

/// <summary>A colour as uniwow.h takes it.</summary>
public static class Colors
{
    public static uint Rgba(byte r, byte g, byte b, byte a = 255) =>
        ((uint)r << 24) | ((uint)g << 16) | ((uint)b << 8) | a;
}

/// <summary>The editor, as the module reaches it.</summary>
public static unsafe class Editor
{
    public const uint ApiVersion = 3;

    static Api* table;
    static Action<string>? applyChange;

    /// <summary>Keeps the table of the C interface; call it first in the entry point.</summary>
    public static void Start(Api* api) => table = api;

    /// <summary>The table, for what these classes do not cover.</summary>
    public static Api* Table => table;

    internal static IntPtr Context => table->Context;

    /// <summary>Hands a text to a reply function the editor gave.</summary>
    public static void Reply(delegate* unmanaged<IntPtr, byte*, void> reply, IntPtr context, string text)
    {
        using var copy = new Utf8(text);
        reply(context, copy.Pointer);
    }

    /// <summary>Fills uniwow_module_info; call it last in the entry point. Its texts are kept
    /// while the module is loaded. <paramref name="applyChange"/> receives the values recorded
    /// with RecordChange, on the module's thread, and throws when it cannot apply one.</summary>
    public static void Describe(ModuleInfo* info, string name, string version, IReadOnlyList<Command> commands,
                                IReadOnlyList<PanelDeclaration> panels, Action<string>? applyChange = null)
    {
        var entries = (NativeCommand*)NativeMemory.AllocZeroed((nuint)Math.Max(commands.Count, 1),
                                                                (nuint)sizeof(NativeCommand));
        for (int i = 0; i < commands.Count; i++)
        {
            var command = commands[i];
            entries[i] = new NativeCommand
            {
                Name = Utf8.Keep(command.Name),
                Description = Utf8.Keep(command.Description),
                ArgumentsSchema = Utf8.Keep(command.ArgumentsSchema),
                ResultSchema = Utf8.Keep(command.ResultSchema),
                Handler = &RunCommand,
                User = GCHandle.ToIntPtr(GCHandle.Alloc(command.Handler)),
            };
        }
        var declared = (NativePanel*)NativeMemory.AllocZeroed((nuint)Math.Max(panels.Count, 1),
                                                              (nuint)sizeof(NativePanel));
        for (int i = 0; i < panels.Count; i++)
        {
            declared[i] = new NativePanel
            {
                Id = Utf8.Keep(panels[i].Id),
                Title = Utf8.Keep(panels[i].Title),
                Area = (uint)panels[i].Area,
            };
        }
        Editor.applyChange = applyChange;
        info->Name = Utf8.Keep(name);
        info->Version = Utf8.Keep(version);
        info->Commands = entries;
        info->CommandCount = (uint)commands.Count;
        info->HeaderVersion = ApiVersion;
        info->CommandSize = (uint)sizeof(NativeCommand);
        info->Panels = declared;
        info->PanelCount = (uint)panels.Count;
        info->ApplyChange = applyChange != null ? &RunApplyChange : null;
        info->User = IntPtr.Zero;
    }

    public static void Log(LogLevel level, string message)
    {
        using var text = new Utf8(message);
        table->Log(Context, (int)level, text.Pointer);
    }

    /// <summary>Calls a command of the editor and waits for it: true and its JSON result, or false
    /// and an error message.</summary>
    public static (bool Ok, string Answer) Call(string name, string argumentsJson = "{}")
    {
        using var command = new Utf8(name);
        using var arguments = new Utf8(argumentsJson);
        using var answer = new Answer();
        int status = table->Call(Context, command.Pointer, arguments.Pointer, Answer.Reply, answer.Context);
        return (status == 0, answer.Text);
    }

    /// <summary>Records a change the module already made, as one undo entry; see record_change
    /// in uniwow.h.</summary>
    public static bool RecordChange(string label, string undoJson, string redoJson)
    {
        using var name = new Utf8(label);
        using var undo = new Utf8(undoJson);
        using var redo = new Utf8(redoJson);
        return table->RecordChange(Context, name.Pointer, undo.Pointer, redo.Pointer) == 0;
    }

    /// <summary>The commands applied by this module from the calling thread until EndGroup form
    /// one undo entry.</summary>
    public static void BeginGroup(string label)
    {
        using var text = new Utf8(label);
        table->BeginGroup(Context, text.Pointer);
    }

    public static void EndGroup() => table->EndGroup(Context);

    /// <summary>A slot connected, under the number the editor hands back with each signal.</summary>
    sealed record Connected(ulong Connection, ulong Sender, Action<SignalData> Slot);

    static readonly object gate = new();
    static long nextSlot = 1;
    static readonly Dictionary<long, Connected> connected = new();
    /// <summary>The parent of each object, to let go what a destroy takes with it.</summary>
    static readonly Dictionary<ulong, ulong> parents = new();
    /// <summary>The panels, which the editor does not destroy.</summary>
    static readonly HashSet<ulong> panels = new();

    /// <summary>Connects a slot receiving the raw signal.</summary>
    public static ulong Connect(ulong sender, SignalId signal, Action<SignalData> slot)
    {
        // Held across Connect: a signal sent at once from another thread waits for its slot.
        lock (gate)
        {
            var key = nextSlot++;
            var connection = table->Connect(Context, sender, (uint)signal, &RunSlot, (IntPtr)key);
            if (connection != 0)
            {
                connected[key] = new Connected(connection, sender, slot);
            }
            return connection;
        }
    }

    public static void Disconnect(ulong connection)
    {
        table->Disconnect(Context, connection);
        Forget(entry => entry.Connection == connection);
    }

    static void Forget(Func<Connected, bool> drop)
    {
        lock (gate)
        {
            var dropped = new List<long>();
            foreach (var (key, entry) in connected)
            {
                if (drop(entry))
                {
                    dropped.Add(key);
                }
            }
            foreach (var key in dropped)
            {
                connected.Remove(key);
            }
        }
    }

    /// <summary>Records that <paramref name="container"/> holds <paramref name="child"/>;
    /// <paramref name="alone"/> for the one layout of a panel, group box or dialog.</summary>
    internal static void Placed(ulong container, ulong child, bool alone)
    {
        lock (gate)
        {
            if (alone)
            {
                var replaced = new List<ulong>();
                foreach (var (held, parent) in parents)
                {
                    if (parent == container)
                    {
                        replaced.Add(held);
                    }
                }
                foreach (var held in replaced)
                {
                    parents.Remove(held);
                }
            }
            parents[child] = container;
        }
    }

    internal static void AddPanel(ulong handle)
    {
        lock (gate)
        {
            panels.Add(handle);
        }
    }

    /// <summary>Lets go the slots connected to a destroyed object and to its children.</summary>
    internal static void Destroyed(ulong handle)
    {
        var gone = new HashSet<ulong> { handle };
        lock (gate)
        {
            if (panels.Contains(handle))
            {
                return;
            }
            for (var grew = true; grew;)
            {
                grew = false;
                foreach (var (child, parent) in parents)
                {
                    if (gone.Contains(parent) && gone.Add(child))
                    {
                        grew = true;
                    }
                }
            }
            foreach (var other in gone)
            {
                parents.Remove(other);
            }
        }
        Forget(entry => gone.Contains(entry.Sender));
    }

    [UnmanagedCallersOnly]
    static void RunSlot(IntPtr user, SignalData* signal)
    {
        Action<SignalData>? slot;
        lock (gate)
        {
            slot = connected.TryGetValue((long)user, out var entry) ? entry.Slot : null;
        }
        if (slot is null)
        {
            return;
        }
        try
        {
            slot(*signal);
        }
        catch (Exception failure)
        {
            Log(LogLevel.Error, "a slot threw: " + failure.Message);
        }
    }

    [UnmanagedCallersOnly]
    static int RunCommand(IntPtr user, byte* arguments, delegate* unmanaged<IntPtr, byte*, void> reply,
                          IntPtr replyContext)
    {
        string answer;
        int status;
        try
        {
            answer = ((Func<string, string>)GCHandle.FromIntPtr(user).Target!)(Utf8.Read(arguments));
            status = 0;
        }
        catch (Exception failure)
        {
            answer = failure.Message;
            status = 1;
        }
        Reply(reply, replyContext, answer);
        return status;
    }

    [UnmanagedCallersOnly]
    static int RunApplyChange(IntPtr user, byte* value, delegate* unmanaged<IntPtr, byte*, void> error,
                              IntPtr errorContext)
    {
        try
        {
            applyChange!(Utf8.Read(value));
            return 0;
        }
        catch (Exception failure)
        {
            Reply(error, errorContext, failure.Message);
            return 1;
        }
    }
}

/// <summary>A reply function and its context, keeping the text the editor replies.</summary>
public sealed unsafe class Answer : IDisposable
{
    GCHandle handle;

    public Answer() => handle = GCHandle.Alloc(this);

    public string Text { get; private set; } = "";

    public IntPtr Context => GCHandle.ToIntPtr(handle);

    public static delegate* unmanaged<IntPtr, byte*, void> Reply => &Collect;

    public void Dispose() => handle.Free();

    [UnmanagedCallersOnly]
    static void Collect(IntPtr context, byte* text) =>
        ((Answer)GCHandle.FromIntPtr(context).Target!).Text = Utf8.Read(text);
}

/// <summary>What a signal of a scene item carries: the item, where it is in the scene, how far
/// it moved, the button (1 left, 2 right, 3 middle) and the keys held (1 Ctrl, 2 Shift, 4 Alt).</summary>
public readonly record struct ItemEvent(ulong Item, double X, double Y, double Dx, double Dy, uint Button,
                                        uint Modifiers);

/// <summary>What a mouse signal of a paint area carries.</summary>
public readonly record struct MouseEvent(double X, double Y, double Dx, double Dy, uint Button, uint Modifiers);

/// <summary>A signal carrying nothing; Connect returns the connection, for Disconnect.</summary>
public sealed class Signal(ulong sender, SignalId id)
{
    public ulong Connect(Action slot) => Editor.Connect(sender, id, _ => slot());

    public static void Disconnect(ulong connection) => Editor.Disconnect(connection);
}

/// <summary>A signal carrying a value.</summary>
public sealed class Signal<T>(ulong sender, SignalId id, Func<SignalData, T> payload)
{
    public ulong Connect(Action<T> slot) => Editor.Connect(sender, id, signal => slot(payload(signal)));

    public static void Disconnect(ulong connection) => Editor.Disconnect(connection);
}

/// <summary>The base of every interface object (QObject).</summary>
public unsafe class UiObject(ulong handle)
{
    public ulong Handle { get; private set; } = handle;

    public void Destroy()
    {
        Table->Destroy(Context, Handle);
        Editor.Destroyed(Handle);
        Handle = 0;
    }

    protected static Api* Table => Editor.Table;

    protected static IntPtr Context => Editor.Table->Context;

    protected static ulong Make(Kind kind, ulong parent = 0)
    {
        var handle = Table->Create(Context, (uint)kind, parent);
        if (handle != 0 && parent != 0)
        {
            Editor.Placed(parent, handle, false);
        }
        return handle;
    }

    /// <summary>Places a widget or layout in this layout, or sets the one layout of this
    /// container.</summary>
    protected void Hold(UiObject child, bool alone, uint row = 0, uint column = 0, uint rowSpan = 1,
                        uint columnSpan = 1)
    {
        if (Table->AddTo(Context, Handle, child.Handle, row, column, rowSpan, columnSpan) == 0)
        {
            Editor.Placed(Handle, child.Handle, alone);
        }
    }

    protected void WriteText(Property property, string text)
    {
        using var copy = new Utf8(text);
        Table->SetText(Context, Handle, (uint)property, copy.Pointer);
    }

    protected string ReadText(Property property)
    {
        using var answer = new Answer();
        Table->Text(Context, Handle, (uint)property, Answer.Reply, answer.Context);
        return answer.Text;
    }

    protected void WriteNumbers(Property property, params ReadOnlySpan<double> values)
    {
        fixed (double* numbers = values)
        {
            Table->SetNumbers(Context, Handle, (uint)property, numbers, (uint)values.Length);
        }
    }

    protected double[] ReadNumbers(Property property)
    {
        double* values = stackalloc double[4];
        uint count = Table->Numbers(Context, Handle, (uint)property, values, 4);
        return new ReadOnlySpan<double>(values, (int)Math.Min(count, 4u)).ToArray();
    }

    protected double ReadNumber(Property property)
    {
        double[] values = ReadNumbers(property);
        return values.Length > 0 ? values[0] : 0.0;
    }

    protected static double Flag(bool value) => value ? 1.0 : 0.0;

    protected static Signal<bool> BoolSignal(ulong sender, SignalId id) => new(sender, id, s => s.Boolean != 0);

    protected static Signal<double> NumberSignal(ulong sender, SignalId id) => new(sender, id, s => s.Number);

    protected static Signal<int> IntSignal(ulong sender, SignalId id) => new(sender, id, s => (int)s.Integer);

    protected static Signal<string> TextSignal(ulong sender, SignalId id) => new(sender, id, s => s.Text);

    protected static Signal<ItemEvent> ItemSignal(ulong sender, SignalId id) =>
        new(sender, id, s => new ItemEvent(s.Item, s.X, s.Y, s.Dx, s.Dy, s.Button, s.Modifiers));

    protected static Signal<MouseEvent> MouseSignal(ulong sender, SignalId id) =>
        new(sender, id, s => new MouseEvent(s.X, s.Y, s.Dx, s.Dy, s.Button, s.Modifiers));
}

public class Widget(ulong handle) : UiObject(handle)
{
    public void SetEnabled(bool enabled) => WriteNumbers(Property.Enabled, Flag(enabled));
    public void SetVisible(bool visible) => WriteNumbers(Property.Visible, Flag(visible));
    public void SetToolTip(string text) => WriteText(Property.ToolTip, text);
}

public class Label : Widget
{
    public Label(string text = "") : base(Make(Kind.Label)) => SetText(text);

    public void SetText(string text) => WriteText(Property.Text, text);
    public string Text() => ReadText(Property.Text);
}

public class PushButton : Widget
{
    public PushButton(string text = "") : base(Make(Kind.PushButton))
    {
        SetText(text);
        Clicked = new Signal(Handle, SignalId.Clicked);
    }

    public Signal Clicked { get; }

    public void SetText(string text) => WriteText(Property.Text, text);
}

public class CheckBox : Widget
{
    public CheckBox(string text = "") : base(Make(Kind.CheckBox))
    {
        WriteText(Property.Text, text);
        Toggled = BoolSignal(Handle, SignalId.Toggled);
    }

    public Signal<bool> Toggled { get; }

    public void SetChecked(bool isChecked) => WriteNumbers(Property.Checked, Flag(isChecked));
    public bool IsChecked() => ReadNumber(Property.Checked) != 0.0;
}

public class Slider : Widget
{
    public Slider() : base(Make(Kind.Slider))
    {
        ValueChanged = NumberSignal(Handle, SignalId.ValueChanged);
        SliderPressed = NumberSignal(Handle, SignalId.SliderPressed);
        SliderReleased = NumberSignal(Handle, SignalId.SliderReleased);
    }

    public Signal<double> ValueChanged { get; }
    public Signal<double> SliderPressed { get; }
    public Signal<double> SliderReleased { get; }

    public void SetRange(double minimum, double maximum)
    {
        WriteNumbers(Property.Minimum, minimum);
        WriteNumbers(Property.Maximum, maximum);
    }

    public void SetSingleStep(double step) => WriteNumbers(Property.Step, step);
    public void SetValue(double value) => WriteNumbers(Property.Value, value);
    public double Value() => ReadNumber(Property.Value);
}

public class SpinBox : Widget
{
    public SpinBox() : base(Make(Kind.SpinBox))
    {
        ValueChanged = NumberSignal(Handle, SignalId.ValueChanged);
        EditingFinished = NumberSignal(Handle, SignalId.EditingFinished);
    }

    public Signal<double> ValueChanged { get; }
    public Signal<double> EditingFinished { get; }

    public void SetRange(double minimum, double maximum)
    {
        WriteNumbers(Property.Minimum, minimum);
        WriteNumbers(Property.Maximum, maximum);
    }

    public void SetSingleStep(double step) => WriteNumbers(Property.Step, step);
    public void SetDecimals(int decimals) => WriteNumbers(Property.Decimals, decimals);
    public void SetValue(double value) => WriteNumbers(Property.Value, value);
    public double Value() => ReadNumber(Property.Value);
}

public class LineEdit : Widget
{
    public LineEdit(string text = "") : base(Make(Kind.LineEdit))
    {
        SetText(text);
        TextChanged = TextSignal(Handle, SignalId.TextChanged);
        EditingFinished = TextSignal(Handle, SignalId.EditingFinished);
    }

    public Signal<string> TextChanged { get; }
    public Signal<string> EditingFinished { get; }

    public void SetText(string text) => WriteText(Property.Text, text);
    public string Text() => ReadText(Property.Text);
    public void SetPlaceholderText(string text) => WriteText(Property.Placeholder, text);
}

public unsafe class ComboBox : Widget
{
    public ComboBox() : base(Make(Kind.ComboBox)) =>
        CurrentIndexChanged = IntSignal(Handle, SignalId.CurrentIndexChanged);

    public Signal<int> CurrentIndexChanged { get; }

    public void AddItem(string text)
    {
        using var copy = new Utf8(text);
        Table->AddEntry(Context, Handle, copy.Pointer);
    }

    public void Clear() => Table->ClearEntries(Context, Handle);
    public int Count() => (int)ReadNumber(Property.Count);
    public void SetCurrentIndex(int index) => WriteNumbers(Property.CurrentIndex, index);
    public int CurrentIndex() => (int)ReadNumber(Property.CurrentIndex);
}

public class Separator() : Widget(Make(Kind.Separator));

public unsafe class Layout(ulong handle) : UiObject(handle)
{
    public void AddWidget(UiObject widget) => Hold(widget, false);
    public void AddLayout(Layout layout) => AddWidget(layout);
}

public class VBoxLayout() : Layout(Make(Kind.VBoxLayout));

public class HBoxLayout() : Layout(Make(Kind.HBoxLayout));

public unsafe class GridLayout() : Layout(Make(Kind.GridLayout))
{
    /// <summary>Rows and columns count from 0, spans from 1, up to 10000.</summary>
    public void AddWidget(UiObject widget, int row, int column, int rowSpan = 1, int columnSpan = 1)
    {
        if (row < 0 || column < 0 || rowSpan < 1 || columnSpan < 1)
        {
            throw new ArgumentOutOfRangeException(nameof(row), "a grid cell has a row and a column from 0 and spans from 1");
        }
        Hold(widget, false, (uint)row, (uint)column, (uint)rowSpan, (uint)columnSpan);
    }
}

public unsafe class GroupBox : Widget
{
    public GroupBox(string title = "") : base(Make(Kind.GroupBox)) => SetTitle(title);

    public void SetTitle(string title) => WriteText(Property.Title, title);
    public void SetLayout(Layout layout) => Hold(layout, true);
}

/// <summary>Curves edited by hand, drawn by the module curves: SetCurves and Curves take the JSON
/// of the property CURVES of uniwow.h; CurvesChanged gives the curves and whether the change is
/// done.</summary>
public class CurveView : Widget
{
    public CurveView() : base(Make(Kind.CurveView)) =>
        CurvesChanged = new Signal<(string Json, bool Finished)>(Handle, SignalId.CurvesChanged,
                                                                   s => (s.Text, s.Boolean != 0));

    public Signal<(string Json, bool Finished)> CurvesChanged { get; }

    public void SetCurves(string json) => WriteText(Property.Curves, json);
    public string Curves() => ReadText(Property.Curves);
    public void SetMinimumHeight(double height) => WriteNumbers(Property.MinimumHeight, height);
}

/// <summary>A modal window, as QDialog: while it is shown, the rest of the editor cannot be
/// used. It is created hidden; the user closing it, with Escape or its close button, hides it and
/// sends Rejected.</summary>
public unsafe class Dialog : UiObject
{
    public Dialog(string title = "") : base(Make(Kind.Dialog))
    {
        SetTitle(title);
        Rejected = new Signal(Handle, SignalId.Rejected);
    }

    public Signal Rejected { get; }

    public void SetTitle(string title) => WriteText(Property.Title, title);
    public void SetLayout(Layout layout) => Hold(layout, true);
    public void Show() => WriteNumbers(Property.Visible, 1.0);
    public void Hide() => WriteNumbers(Property.Visible, 0.0);
}

/// <summary>A dock panel the module declared with Editor.Describe.</summary>
public unsafe class Panel(string id) : UiObject(Find(id))
{
    public void SetLayout(Layout layout) => Hold(layout, true);

    static ulong Find(string id)
    {
        using var copy = new Utf8(id);
        var handle = Table->Panel(Context, copy.Pointer);
        Editor.AddPanel(handle);
        return handle;
    }
}

public class GraphicsScene : UiObject
{
    public GraphicsScene() : base(Make(Kind.GraphicsScene))
    {
        ItemPressed = ItemSignal(Handle, SignalId.ItemPressed);
        ItemMoved = ItemSignal(Handle, SignalId.ItemMoved);
        ItemDoubleClicked = ItemSignal(Handle, SignalId.ItemDoubleClicked);
        SelectionChanged = new Signal(Handle, SignalId.SelectionChanged);
    }

    public Signal<ItemEvent> ItemPressed { get; }
    public Signal<ItemEvent> ItemMoved { get; }
    public Signal<ItemEvent> ItemDoubleClicked { get; }
    public Signal SelectionChanged { get; }
}

/// <summary>The base of the items of a scene; their parent is the scene or an item group.</summary>
public class GraphicsItem(ulong handle) : UiObject(handle)
{
    [Flags]
    public enum Flags : uint { ItemIsMovableX = 1, ItemIsMovableY = 2, ItemIsMovable = 3 }

    public void SetPos(double x, double y) => WriteNumbers(Property.Pos, x, y);

    public (double X, double Y) Pos()
    {
        double[] values = ReadNumbers(Property.Pos);
        return (values.Length > 0 ? values[0] : 0.0, values.Length > 1 ? values[1] : 0.0);
    }

    public void SetZValue(double z) => WriteNumbers(Property.ZValue, z);
    public void SetMovable(Flags flags) => WriteNumbers(Property.Movable, (double)flags);
    public void SetMoveBounds(double x, double y, double width, double height) =>
        WriteNumbers(Property.MoveBounds, x, y, width, height);
    public void SetSelectable(bool selectable) => WriteNumbers(Property.Selectable, Flag(selectable));
    public void SetSelected(bool selected) => WriteNumbers(Property.Selected, Flag(selected));
    public bool IsSelected() => ReadNumber(Property.Selected) != 0.0;
    public void SetVisible(bool visible) => WriteNumbers(Property.Visible, Flag(visible));
    public void SetToolTip(string text) => WriteText(Property.ToolTip, text);

    public void SetPen(uint color, double width = 1.0)
    {
        WriteNumbers(Property.PenColor, color);
        WriteNumbers(Property.PenWidth, width);
    }

    public void SetBrush(uint color) => WriteNumbers(Property.BrushColor, color);
}

public class RectItem : GraphicsItem
{
    public RectItem(UiObject parent, double x, double y, double width, double height)
        : base(Make(Kind.RectItem, parent.Handle)) => SetRect(x, y, width, height);

    public void SetRect(double x, double y, double width, double height) =>
        WriteNumbers(Property.Rect, x, y, width, height);

    public void SetRadius(double radius) => WriteNumbers(Property.Radius, radius);
}

public class EllipseItem : GraphicsItem
{
    public EllipseItem(UiObject parent, double x, double y, double width, double height)
        : base(Make(Kind.EllipseItem, parent.Handle)) => WriteNumbers(Property.Rect, x, y, width, height);
}

public class LineItem : GraphicsItem
{
    public LineItem(UiObject parent, double x1, double y1, double x2, double y2)
        : base(Make(Kind.LineItem, parent.Handle)) => SetLine(x1, y1, x2, y2);

    public void SetLine(double x1, double y1, double x2, double y2) => WriteNumbers(Property.Line, x1, y1, x2, y2);
}

public class TextItem : GraphicsItem
{
    public TextItem(UiObject parent, string text) : base(Make(Kind.TextItem, parent.Handle)) => SetText(text);

    public void SetText(string text) => WriteText(Property.Text, text);
    public void SetFontSize(double size) => WriteNumbers(Property.FontSize, size);
    public void SetColor(uint color) => WriteNumbers(Property.PenColor, color);
}

public class ItemGroup(UiObject parent) : GraphicsItem(Make(Kind.ItemGroup, parent.Handle));

public unsafe class GraphicsView() : Widget(Make(Kind.GraphicsView))
{
    public void SetScene(GraphicsScene scene) => Table->SetScene(Context, Handle, scene.Handle);
    public void SetMinimumHeight(double height) => WriteNumbers(Property.MinimumHeight, height);
    public void SetScale(double scale) => WriteNumbers(Property.ViewScale, scale);
    public void CenterOn(double x, double y) => WriteNumbers(Property.ViewCenter, x, y);
}

/// <summary>Paints a paint area, inside its paint function (QPainter).</summary>
public readonly unsafe struct Painter(ulong painter)
{
    static Api* Table => Editor.Table;

    static IntPtr Context => Editor.Table->Context;

    public void SetPen(uint color, double width = 1.0) => Table->SetPen(Context, painter, color, width);
    public void SetBrush(uint color) => Table->SetBrush(Context, painter, color);
    public void DrawLine(double x1, double y1, double x2, double y2) =>
        Table->DrawLine(Context, painter, x1, y1, x2, y2);
    public void DrawRect(double x, double y, double width, double height, double radius = 0.0) =>
        Table->DrawRect(Context, painter, x, y, width, height, radius);
    public void DrawEllipse(double x, double y, double width, double height) =>
        Table->DrawEllipse(Context, painter, x, y, width, height);

    /// <summary>Draws a text whose top left corner is at x, y.</summary>
    public void DrawText(double x, double y, string text, double size = 13.0)
    {
        using var copy = new Utf8(text);
        Table->DrawText(Context, painter, x, y, copy.Pointer, size);
    }

    public void Translate(double dx, double dy) => Table->Translate(Context, painter, dx, dy);
    public void Scale(double sx, double sy) => Table->Scale(Context, painter, sx, sy);
    public void Save() => Table->Save(Context, painter);
    public void Restore() => Table->Restore(Context, painter);
}

/// <summary>A widget the module paints, as a QWidget with its paintEvent.</summary>
public unsafe class PaintArea : Widget
{
    public PaintArea() : base(Make(Kind.PaintArea))
    {
        MousePressed = MouseSignal(Handle, SignalId.MousePress);
        MouseMoved = MouseSignal(Handle, SignalId.MouseMove);
        MouseReleased = MouseSignal(Handle, SignalId.MouseRelease);
        Wheel = MouseSignal(Handle, SignalId.Wheel);
    }

    public Signal<MouseEvent> MousePressed { get; }
    public Signal<MouseEvent> MouseMoved { get; }
    public Signal<MouseEvent> MouseReleased { get; }
    public Signal<MouseEvent> Wheel { get; }

    public void SetMinimumHeight(double height) => WriteNumbers(Property.MinimumHeight, height);

    /// <summary>Asks for the paint function again.</summary>
    public void Update() => Table->Update(Context, Handle);

    /// <summary>Sets how the area is painted: called with a painter, the width and the height.</summary>
    public ulong Paint(Action<Painter, double, double> paint) =>
        Editor.Connect(Handle, SignalId.Paint,
                       signal => paint(new Painter(signal.Painter), signal.Width, signal.Height));
}
