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

    public delegate* unmanaged<IntPtr, delegate* unmanaged<IntPtr, byte*, void>, IntPtr, void> Properties;
    public delegate* unmanaged<IntPtr, byte*, double*, uint, uint> ReadProperty;
    public delegate* unmanaged<IntPtr, byte*, double*, uint, int> WriteProperty;
    public delegate* unmanaged<IntPtr, byte*, double*, uint, int> SetProperty;

    public delegate* unmanaged<IntPtr, ulong, ulong, uint, byte*, int> SetCell;
    public delegate* unmanaged<IntPtr, ulong, uint, byte*, int> InsertRows;
    public delegate* unmanaged<IntPtr, ulong, ulong*, uint, int> RemoveRows;
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
    public NativeProperty* Properties;
    public uint PropertyCount, PropertySize;
}

/// <summary>uniwow_property of uniwow.h.</summary>
[StructLayout(LayoutKind.Sequential)]
public unsafe struct NativeProperty
{
    public byte* Name, Label;
    public uint Kind;
    public double Minimum, Maximum;
    public fixed double Initial[3];
    public delegate* unmanaged<IntPtr, double*, uint, delegate* unmanaged<IntPtr, byte*, void>, IntPtr, int> Write;
    public IntPtr User;
}

// <generated kind> from sdk/bindings.toml by cargo xtask bindings
/// <summary>Kinds of interface objects, named as in Qt.</summary>
public enum Kind : uint
{
    Panel = 1,          // a dock panel; obtained with panel(), never created
    Label = 2,
    PushButton = 3,
    CheckBox = 4,
    Slider = 5,
    SpinBox = 6,
    LineEdit = 7,
    ComboBox = 8,
    Separator = 9,
    GroupBox = 10,
    VBoxLayout = 11,
    HBoxLayout = 12,
    GridLayout = 13,
    GraphicsView = 14,
    GraphicsScene = 15,
    RectItem = 16,
    LineItem = 17,
    EllipseItem = 18,
    TextItem = 19,
    ItemGroup = 20,
    PaintArea = 21,
    Dialog = 22,        // a modal window; created hidden, shown and hidden through VISIBLE
    CurveView = 23,     // curves edited by hand, drawn by the module curves
    Sequence = 24,      // tracks of keys on animatable properties, with a frame rate and a length; not drawn
    Player = 25,        // plays a sequence, as QTimeLine; moved on by the kernel; not drawn
    DopesheetView = 26, // the keys of a sequence edited by hand, and the playhead of a player, drawn by the module
                        // dopesheet
    TreeView = 27,      // items with a text and children, folded or unfolded, one current, as QTreeWidget
    TableView = 28,     // rows of cells under headers, edited in place and sorted by a column, as QTableWidget; only
                        // the rows in sight are drawn
}
// </generated kind>

// <generated property> from sdk/bindings.toml by cargo xtask bindings
/// <summary>Properties. Texts go through set_text; everything else through set_numbers: a flag is 0 or 1, a
/// colour is 0xRRGGBBAA, positions and sizes are in points.</summary>
public enum Property : uint
{
    Text = 1,
    ToolTip = 2,
    Enabled = 3,
    Visible = 4,
    Checked = 5,
    Value = 6,           // slider, spin box; kept within the range
    Minimum = 7,
    Maximum = 8,
    Step = 9,
    Decimals = 10,
    Placeholder = 11,    // line edit, text
    CurrentIndex = 12,   // combo box
    Title = 13,          // group box, text
    Pos = 14,            // item: x, y in its parent
    Rect = 15,           // rectangle or ellipse item: x, y, width, height
    Line = 16,           // line item: x1, y1, x2, y2
    PenColor = 17,
    PenWidth = 18,
    BrushColor = 19,
    Radius = 20,         // rectangle item: corner radius
    ZValue = 21,         // item: stacking order among its siblings
    Movable = 22,        // item: 0 no, 1 along x, 2 along y, 3 both
    Selectable = 23,
    Selected = 24,
    MoveBounds = 25,     // movable item: x, y, width, height its position stays in
    FontSize = 26,       // text item, 1 to 512, finite
    MinimumHeight = 27,  // graphics view, paint area
    ViewScale = 28,      // graphics view: zoom
    ViewCenter = 29,     // graphics view: x, y of the scene at its centre
    Count = 30,          // read only: entries of a combo box, children otherwise
    Curves = 31,         // curve view, text: JSON [{label, colour: [r, g, b], visible, keys: [{time, value, mode, left,
                         // right}]}]; keys in time order, each at a time of its own, numbers within 1e9
    Tracks = 32,         // sequence, text: JSON [{property, kind, curves: [{keys: [{time, value, mode, left,
                         // right}]}]}], one curve per number of the property; keys at whole frames from 0, numbers
                         // within 1e9; each change is an undo entry the kernel records
    FrameRate = 33,      // sequence: frames per second, a whole number from 1 to 240
    Length = 34,         // sequence: in frames, a whole number from 1 to 1000000
    Sequence = 35,       // player, dopesheet view, curve view: the handle of the sequence it plays or shows, 0 for
                         // none; a view changes it directly, each change done an undo entry the kernel records
    Time = 36,           // player: in frames, fractional, from 0 to the length of its sequence
    Playing = 37,        // player: 1 plays from the time, or from 0 when at the end; 0 pauses
    Loop = 38,           // player: at the end, starts again from 0
    Speed = 39,          // player: times the frame rate, from 0 to 100
    Player = 40,         // dopesheet view, curve view: the handle of the player whose time it shows as the playhead, 0
                         // for none
    Items = 41,          // tree view, text: JSON [{id, text, expanded, children: [...]}]; ids whole numbers from 1,
                         // each of its own; 64 levels and 1000000 items at most
    Columns = 42,        // table view, text: JSON [header, ...], 1000 columns at most
    Rows = 43,           // table view, text: JSON [{id, cells: [text, ...]}] in the module's order; ids whole numbers
                         // from 1, each of its own; 1000000 rows at most
    CurrentItem = 44,    // tree view, table view: the id of the current item or row, 0 for none; an id the view does
                         // not hold is refused, and an item or row removed is no longer current
    SortColumn = 45,     // table view: the column the rows are shown sorted by, -1 for the module's order
    SortDescending = 46, // table view: sorted from the highest
}
// </generated property>

// <generated signal> from sdk/bindings.toml by cargo xtask bindings
/// <summary>Signals, named as in Qt. Each tells what the user did, never a change the module made.</summary>
public enum SignalId : uint
{
    Clicked = 1,             // push button
    Toggled = 2,             // check box: boolean
    ValueChanged = 3,        // slider, spin box: number
    SliderPressed = 4,       // slider: number
    SliderReleased = 5,      // slider: number; also after a click or a key
    TextChanged = 6,         // line edit: text
    EditingFinished = 7,     // line edit: text; spin box: number
    CurrentIndexChanged = 8, // combo box: integer
    ItemPressed = 9,         // scene: item, x, y in the scene, button, modifiers
    ItemMoved = 10,          // scene: item dropped at x, y, moved by dx, dy
    ItemDoubleClicked = 11,  // scene: item, x, y
    SelectionChanged = 12,   // scene; read each item's SELECTED
    Paint = 13,              // paint area: painter, width, height
    MousePress = 14,         // paint area: x, y, button, modifiers
    MouseMove = 15,          // paint area, while a button is held: x, y
    MouseRelease = 16,       // paint area: x, y
    Wheel = 17,              // paint area: x, y, dx, dy
    Rejected = 18,           // dialog: the user closed it, which hid it
    CurvesChanged = 19,      // curve view: text, the curves; boolean, whether the change is done
    TimeChanged = 20,        // player, as it plays: number, the time in frames; its slot records nothing and Undo does
                             // not wait for it
    Finished = 21,           // player: it reached the end without LOOP and stopped
    KeysChanged = 22,        // dopesheet view, curve view showing a sequence: text, the tracks; boolean, whether the
                             // change is done, then made to the sequence
    PlayheadMoved = 23,      // dopesheet view: number, the frame the user moved the playhead to, its player paused
                             // there
    ItemClicked = 24,        // tree view: item, the id of the item clicked
    CurrentItemChanged = 25, // tree view: item, the id of the item now current
    ItemExpanded = 26,       // tree view: item, the id of the item unfolded or folded; boolean, whether unfolded
    CellChanged = 27,        // table view: item, the id of the row; integer, the column; text, the cell edited by hand
    CurrentCellChanged = 28, // table view: item, the id of the row; integer, the column of the cell now current
    SortChanged = 29,        // table view: integer, the column whose header was clicked, now sorted by; boolean,
                             // whether from the highest
}
// </generated signal>

// <generated value_kind> from sdk/bindings.toml by cargo xtask bindings
/// <summary>Types of the values of animatable properties.</summary>
public enum ValueKind : uint
{
    Number = 1,
    Vector = 2,  // three numbers, such as a position
    Colour = 3,  // red, green and blue, from 0 to 1
    Boolean = 4, // one number, 0 or 1
}
// </generated value_kind>

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
    public const uint ApiVersion = 5;

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
        var properties = (NativeProperty*)NativeMemory.AllocZeroed((nuint)Math.Max(declaredProperties.Count, 1),
                                                                   (nuint)sizeof(NativeProperty));
        for (int i = 0; i < declaredProperties.Count; i++)
        {
            var property = declaredProperties[i];
            properties[i].Name = Utf8.Keep(property.Name);
            properties[i].Label = Utf8.Keep(property.Label);
            properties[i].Kind = (uint)property.Kind;
            properties[i].Minimum = property.Minimum;
            properties[i].Maximum = property.Maximum;
            for (int n = 0; n < Math.Min(property.Initial.Length, 3); n++)
            {
                properties[i].Initial[n] = property.Initial[n];
            }
            properties[i].Write = &RunWrite;
            properties[i].User = (IntPtr)i;
        }
        info->Properties = properties;
        info->PropertyCount = (uint)declaredProperties.Count;
        info->PropertySize = (uint)sizeof(NativeProperty);
    }

    /// <summary>An animatable property declared with DeclareProperty.</summary>
    sealed record DeclaredProperty(string Name, string Label, ValueKind Kind, double Minimum, double Maximum,
                                   double[] Initial, Action<double[]> Write);

    static readonly List<DeclaredProperty> declaredProperties = [];

    /// <summary>Declares an animatable property, `module/name`, before Describe. Its value is one
    /// number, or three for a vector or a colour; <paramref name="initial"/> is the one it has at
    /// start. <paramref name="write"/> receives each value written from elsewhere, on the module's
    /// thread, and may change it into the value it keeps; throwing makes the module fail. It records
    /// nothing: RecordChange and the undo groups are refused while it runs.</summary>
    public static void DeclareProperty(string name, string label, ValueKind kind, double minimum, double maximum,
                                       double[] initial, Action<double[]> write) =>
        declaredProperties.Add(new DeclaredProperty(name, label, kind, minimum, maximum, initial, write));

    /// <summary>The properties of the running modules, as the JSON of properties in uniwow.h.</summary>
    public static string Properties()
    {
        using var answer = new Answer();
        table->Properties(Context, Answer.Reply, answer.Context);
        return answer.Text;
    }

    /// <summary>The numbers of a property; none when refused.</summary>
    public static double[] ReadProperty(string path)
    {
        using var text = new Utf8(path);
        var values = new double[3];
        uint count;
        fixed (double* numbers = values)
        {
            count = table->ReadProperty(Context, text.Pointer, numbers, 3);
        }
        return values[..(int)Math.Min(count, 3)];
    }

    /// <summary>Writes a property, without the history; false when refused.</summary>
    public static bool WriteProperty(string path, params double[] values)
    {
        using var text = new Utf8(path);
        fixed (double* numbers = values)
        {
            return table->WriteProperty(Context, text.Pointer, numbers, (uint)values.Length) == 0;
        }
    }

    /// <summary>Tells the value one of the module's own properties now has; false when
    /// refused.</summary>
    public static bool SetProperty(string name, params double[] values)
    {
        using var text = new Utf8(name);
        fixed (double* numbers = values)
        {
            return table->SetProperty(Context, text.Pointer, numbers, (uint)values.Length) == 0;
        }
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
    static int RunWrite(IntPtr user, double* values, uint count, delegate* unmanaged<IntPtr, byte*, void> error,
                        IntPtr errorContext)
    {
        try
        {
            var numbers = new double[count];
            for (int n = 0; n < count; n++)
            {
                numbers[n] = values[n];
            }
            declaredProperties[(int)user].Write(numbers);
            for (int n = 0; n < Math.Min(count, numbers.Length); n++)
            {
                values[n] = numbers[n];
            }
            return 0;
        }
        catch (Exception failure)
        {
            Reply(error, errorContext, failure.Message);
            return 1;
        }
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

/// <summary>What a signal of a cell of a table view carries: the id of its row, its column, and the
/// text edited by hand for CellChanged.</summary>
public readonly record struct CellEvent(ulong Row, int Column, string Text);

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
/// done. Shown with a sequence, it shows the curves of its tracks instead and changes them
/// directly, each change done an undo entry the editor records; KeysChanged then gives the tracks
/// and whether the change is done.</summary>
public class CurveView : Widget
{
    public CurveView() : base(Make(Kind.CurveView))
    {
        CurvesChanged = new Signal<(string Json, bool Finished)>(Handle, SignalId.CurvesChanged,
                                                                   s => (s.Text, s.Boolean != 0));
        KeysChanged = new Signal<(string Json, bool Finished)>(Handle, SignalId.KeysChanged,
                                                                 s => (s.Text, s.Boolean != 0));
    }

    public Signal<(string Json, bool Finished)> CurvesChanged { get; }
    public Signal<(string Json, bool Finished)> KeysChanged { get; }

    public void SetCurves(string json) => WriteText(Property.Curves, json);
    public string Curves() => ReadText(Property.Curves);
    public void SetSequence(Sequence sequence) => WriteNumbers(Property.Sequence, sequence.Handle);
    /// <summary>The player whose time is shown as the playhead.</summary>
    public void SetPlayer(Player player) => WriteNumbers(Property.Player, player.Handle);
    public void SetMinimumHeight(double height) => WriteNumbers(Property.MinimumHeight, height);
}

/// <summary>The keys of a sequence, drawn by the module dopesheet: a row per track, unfolding into
/// one per number, the keys selected, moved and deleted by hand, each change done an undo entry the
/// editor records; and the playhead of a player, moved by hand on the ruler. KeysChanged gives the
/// tracks and whether the change is done, PlayheadMoved the frame the player was paused
/// at.</summary>
public class DopesheetView : Widget
{
    public DopesheetView() : base(Make(Kind.DopesheetView))
    {
        KeysChanged = new Signal<(string Json, bool Finished)>(Handle, SignalId.KeysChanged,
                                                                 s => (s.Text, s.Boolean != 0));
        PlayheadMoved = NumberSignal(Handle, SignalId.PlayheadMoved);
    }

    public Signal<(string Json, bool Finished)> KeysChanged { get; }
    public Signal<double> PlayheadMoved { get; }

    public void SetSequence(Sequence sequence) => WriteNumbers(Property.Sequence, sequence.Handle);
    public void SetPlayer(Player player) => WriteNumbers(Property.Player, player.Handle);
    /// <summary>The name of the row of every key.</summary>
    public void SetTitle(string title) => WriteText(Property.Title, title);
    public void SetMinimumHeight(double height) => WriteNumbers(Property.MinimumHeight, height);
}

/// <summary>Items with a text and children, as QTreeWidget: SetItems and Items take the JSON of
/// the property ITEMS of uniwow.h, each item with an id of its own from 1. A click on an item makes
/// it current; the triangle before an item folds or unfolds its children, kept in the
/// items.</summary>
public class TreeView : Widget
{
    public TreeView() : base(Make(Kind.TreeView))
    {
        ItemClicked = new Signal<ulong>(Handle, SignalId.ItemClicked, s => s.Item);
        CurrentItemChanged = new Signal<ulong>(Handle, SignalId.CurrentItemChanged, s => s.Item);
        ItemExpanded = new Signal<(ulong Item, bool Expanded)>(Handle, SignalId.ItemExpanded,
                                                                s => (s.Item, s.Boolean != 0));
    }

    public Signal<ulong> ItemClicked { get; }
    public Signal<ulong> CurrentItemChanged { get; }
    public Signal<(ulong Item, bool Expanded)> ItemExpanded { get; }

    public void SetItems(string json) => WriteText(Property.Items, json);
    public string Items() => ReadText(Property.Items);
    /// <summary>0 for none.</summary>
    public void SetCurrentItem(ulong id) => WriteNumbers(Property.CurrentItem, id);
    public ulong CurrentItem() => (ulong)ReadNumber(Property.CurrentItem);
    public void SetMinimumHeight(double height) => WriteNumbers(Property.MinimumHeight, height);
}

/// <summary>Rows of cells under headers, as QTableWidget: SetColumns takes the JSON of the property
/// COLUMNS of uniwow.h, SetRows and Rows the JSON of ROWS, each row with an id of its own from 1,
/// in the module's order. Only the rows in sight are drawn. A click on a header shows the rows
/// sorted by its column, then from the highest, the module's order kept. A double click edits a
/// cell in place: CellChanged gives the text, kept in the rows.</summary>
public unsafe class TableView : Widget
{
    public TableView() : base(Make(Kind.TableView))
    {
        CellChanged = CellSignal(SignalId.CellChanged);
        CurrentCellChanged = CellSignal(SignalId.CurrentCellChanged);
        SortChanged = new Signal<(int Column, bool Descending)>(Handle, SignalId.SortChanged,
                                                                 s => ((int)s.Integer, s.Boolean != 0));
    }

    public Signal<CellEvent> CellChanged { get; }
    public Signal<CellEvent> CurrentCellChanged { get; }
    public Signal<(int Column, bool Descending)> SortChanged { get; }

    public void SetColumns(string json) => WriteText(Property.Columns, json);
    public string Columns() => ReadText(Property.Columns);
    public void SetRows(string json) => WriteText(Property.Rows, json);
    public string Rows() => ReadText(Property.Rows);

    /// <summary>One cell of the row of id <paramref name="row"/>, without giving the rows
    /// again.</summary>
    public bool SetCell(ulong row, int column, string text)
    {
        if (column < 0)
        {
            return false;
        }
        using var copy = new Utf8(text);
        return Table->SetCell(Context, Handle, row, (uint)column, copy.Pointer) == 0;
    }

    /// <summary>Rows inserted at <paramref name="at"/> in the module's order, given as the JSON of
    /// ROWS.</summary>
    public bool InsertRows(int at, string json)
    {
        if (at < 0)
        {
            return false;
        }
        using var copy = new Utf8(json);
        return Table->InsertRows(Context, Handle, (uint)at, copy.Pointer) == 0;
    }

    public bool RemoveRows(params ReadOnlySpan<ulong> ids)
    {
        fixed (ulong* rows = ids)
        {
            return Table->RemoveRows(Context, Handle, rows, (uint)ids.Length) == 0;
        }
    }

    /// <summary>The id of the current row, 0 for none.</summary>
    public void SetCurrentRow(ulong id) => WriteNumbers(Property.CurrentItem, id);
    public ulong CurrentRow() => (ulong)ReadNumber(Property.CurrentItem);

    /// <summary>-1 for the module's order.</summary>
    public void SortByColumn(int column, bool descending = false)
    {
        WriteNumbers(Property.SortColumn, column);
        WriteNumbers(Property.SortDescending, Flag(descending));
    }

    public int SortColumn() => (int)ReadNumber(Property.SortColumn);
    public bool SortDescending() => ReadNumber(Property.SortDescending) != 0.0;
    public void SetMinimumHeight(double height) => WriteNumbers(Property.MinimumHeight, height);

    Signal<CellEvent> CellSignal(SignalId id) =>
        new(Handle, id, s => new CellEvent(s.Item, (int)s.Integer, s.Text));
}

/// <summary>Tracks of keys on animatable properties, with a frame rate and a length, as in the
/// files of the Timeline: SetTracks and Tracks take the JSON of the property TRACKS of uniwow.h.
/// Each change of the tracks is an undo entry the editor records: the module records
/// nothing.</summary>
public class Sequence() : UiObject(Make(Kind.Sequence))
{
    public void SetTracks(string json) => WriteText(Property.Tracks, json);
    public string Tracks() => ReadText(Property.Tracks);
    public void SetFrameRate(int framesPerSecond) => WriteNumbers(Property.FrameRate, framesPerSecond);
    public int FrameRate() => (int)ReadNumber(Property.FrameRate);
    public void SetLength(int frames) => WriteNumbers(Property.Length, frames);
    public int Length() => (int)ReadNumber(Property.Length);
}

/// <summary>Plays a sequence, as QTimeLine: the editor moves it on at each frame and writes the
/// value of each track at its time into its property. TimeChanged gives the time in frames as it
/// plays; its slot records nothing (RecordChange and the undo groups are refused there), so that
/// Undo stays available while it plays. Finished comes at the end, without loop.</summary>
public class Player : UiObject
{
    public Player() : base(Make(Kind.Player))
    {
        TimeChanged = NumberSignal(Handle, SignalId.TimeChanged);
        Finished = new Signal(Handle, SignalId.Finished);
    }

    public Signal<double> TimeChanged { get; }
    public Signal Finished { get; }

    public void SetSequence(Sequence sequence) => WriteNumbers(Property.Sequence, sequence.Handle);
    /// <summary>In frames, fractional.</summary>
    public void SetTime(double frames) => WriteNumbers(Property.Time, frames);
    public double Time() => ReadNumber(Property.Time);
    /// <summary>From the time, or from 0 when at the end.</summary>
    public void Play() => WriteNumbers(Property.Playing, 1.0);
    public void Pause() => WriteNumbers(Property.Playing, 0.0);
    public bool IsPlaying() => ReadNumber(Property.Playing) != 0.0;
    public void SetLoop(bool loop) => WriteNumbers(Property.Loop, Flag(loop));
    public bool Loops() => ReadNumber(Property.Loop) != 0.0;
    /// <summary>Times the frame rate, from 0 to 100.</summary>
    public void SetSpeed(double speed) => WriteNumbers(Property.Speed, speed);
    public double Speed() => ReadNumber(Property.Speed);
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
