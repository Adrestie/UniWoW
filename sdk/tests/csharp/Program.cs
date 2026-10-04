// Tests of the slots of UniWoW.cs over a fake table: a disconnect or a destroy lets the
// connected slot go, and a late call reaches nothing.
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using UniWoW;

static unsafe class Fake
{
    static ulong nextHandle = 1;
    static ulong nextConnection = 1;
    public static readonly Dictionary<ulong, (IntPtr Slot, IntPtr User)> Connections = new();

    [UnmanagedCallersOnly] public static ulong Panel(IntPtr context, byte* id) => 1000;
    [UnmanagedCallersOnly] public static ulong Create(IntPtr context, uint kind, ulong parent) => nextHandle++;
    [UnmanagedCallersOnly] public static void Destroy(IntPtr context, ulong handle) { }

    [UnmanagedCallersOnly]
    public static int AddTo(IntPtr context, ulong container, ulong child, uint row, uint column, uint rowSpan,
                            uint columnSpan) => 0;

    [UnmanagedCallersOnly] public static int SetText(IntPtr context, ulong handle, uint property, byte* text) => 0;

    [UnmanagedCallersOnly]
    public static ulong Connect(IntPtr context, ulong sender, uint signal,
                                delegate* unmanaged<IntPtr, SignalData*, void> slot, IntPtr user)
    {
        Connections[nextConnection] = ((IntPtr)slot, user);
        return nextConnection++;
    }

    [UnmanagedCallersOnly]
    public static void Disconnect(IntPtr context, ulong connection) => Connections.Remove(connection);

    [UnmanagedCallersOnly] public static void Log(IntPtr context, int level, byte* text) { }

    public static string ToldName = "";
    public static double ToldValue;

    [UnmanagedCallersOnly]
    public static int SetProperty(IntPtr context, byte* name, double* values, uint count)
    {
        ToldName = Utf8.Read(name);
        ToldValue = count == 1 ? values[0] : -1;
        return 0;
    }

    public static string Error = "";

    [UnmanagedCallersOnly]
    public static void Collect(IntPtr context, byte* text) => Error = Utf8.Read(text);

    /// <summary>The last numbers set: on which object, which property, and the first number.</summary>
    public static (ulong Handle, uint Property, double Value) Numbers;

    [UnmanagedCallersOnly]
    public static int SetNumbers(IntPtr context, ulong handle, uint property, double* values, uint count)
    {
        Numbers = (handle, property, count > 0 ? values[0] : -1);
        return 0;
    }

    /// <summary>The last cell set and the last ids removed from a table view.</summary>
    public static (ulong Row, uint Column, string Text) Cell = (0, 0, "");
    public static int CellCalls;
    public static ulong[] Removed = [];

    [UnmanagedCallersOnly]
    public static int SetCell(IntPtr context, ulong table, ulong row, uint column, byte* text)
    {
        Cell = (row, column, Utf8.Read(text));
        CellCalls++;
        return 0;
    }

    [UnmanagedCallersOnly]
    public static int RemoveRows(IntPtr context, ulong table, ulong* rows, uint count)
    {
        Removed = new ReadOnlySpan<ulong>(rows, (int)count).ToArray();
        return 0;
    }

    /// <summary>Calls a slot with the data of a signal.</summary>
    public static void CallWith((IntPtr Slot, IntPtr User) kept, SignalData data) =>
        ((delegate* unmanaged<IntPtr, SignalData*, void>)kept.Slot)(kept.User, &data);

    /// <summary>Calls a slot as the editor would, even after its disconnect.</summary>
    public static void Call((IntPtr Slot, IntPtr User) kept, double number = 0)
    {
        var data = new SignalData { Number = number };
        ((delegate* unmanaged<IntPtr, SignalData*, void>)kept.Slot)(kept.User, &data);
    }

    /// <summary>Calls a slot with a text and a boolean, as for a change of keys.</summary>
    public static void CallWithText((IntPtr Slot, IntPtr User) kept, string text, bool boolean)
    {
        using var copy = new Utf8(text);
        var data = new SignalData { TextPointer = copy.Pointer, Boolean = boolean ? 1 : 0 };
        ((delegate* unmanaged<IntPtr, SignalData*, void>)kept.Slot)(kept.User, &data);
    }
}

static unsafe class Program
{
    static int failures;
    static int calls;

    static void Expect(bool ok, string what)
    {
        Console.WriteLine((ok ? "ok   " : "FAIL ") + what);
        failures += ok ? 0 : 1;
    }

    /// <summary>Connects a counting slot; the weak reference tells whether the slot was let go.</summary>
    [MethodImpl(MethodImplOptions.NoInlining)]
    static (ulong Connection, WeakReference Slot) Counted(PushButton button)
    {
        var target = new object();
        var connection = button.Clicked.Connect(() =>
        {
            GC.KeepAlive(target);
            calls++;
        });
        return (connection, new WeakReference(target));
    }

    static bool LetGo(WeakReference slot)
    {
        GC.Collect();
        GC.WaitForPendingFinalizers();
        GC.Collect();
        return !slot.IsAlive;
    }

    static int Main()
    {
        var api = (Api*)NativeMemory.AllocZeroed((nuint)sizeof(Api));
        api->Version = Editor.ApiVersion;
        api->Panel = &Fake.Panel;
        api->Create = &Fake.Create;
        api->Destroy = &Fake.Destroy;
        api->AddTo = &Fake.AddTo;
        api->SetText = &Fake.SetText;
        api->SetNumbers = &Fake.SetNumbers;
        api->Connect = &Fake.Connect;
        api->Disconnect = &Fake.Disconnect;
        api->Log = &Fake.Log;
        api->SetProperty = &Fake.SetProperty;
        api->SetCell = &Fake.SetCell;
        api->RemoveRows = &Fake.RemoveRows;
        Editor.Start(api);

        var panel = new Panel("main");
        var layout = new VBoxLayout();
        panel.SetLayout(layout);
        var button = new PushButton("a");
        layout.AddWidget(button);

        var (first, firstSlot) = Counted(button);
        var kept = Fake.Connections[first];
        Fake.Call(kept);
        Expect(calls == 1, "a connected slot is called");
        Signal.Disconnect(first);
        Expect(LetGo(firstSlot), "disconnect lets the slot go");
        Fake.Call(kept);
        Expect(calls == 1, "a late call after disconnect reaches nothing");

        var (_, inLayout) = Counted(button);
        var other = new PushButton("b");
        var (_, outside) = Counted(other);
        layout.Destroy();
        Expect(LetGo(inLayout) && !LetGo(outside), "destroying a layout lets its widgets' slots go, not the others'");

        var second = new VBoxLayout();
        var inside = new PushButton("c");
        second.AddWidget(inside);
        var (_, insideSlot) = Counted(inside);
        panel.Destroy();
        Expect(!LetGo(insideSlot), "a panel, which the editor keeps, lets nothing go");

        other.Destroy();
        Expect(LetGo(outside), "destroying the sender lets its slot go");

        var box = new GroupBox("g");
        var oldLayout = new VBoxLayout();
        var newLayout = new VBoxLayout();
        box.SetLayout(oldLayout);
        var movedOut = new PushButton("d");
        oldLayout.AddWidget(movedOut);
        var (_, movedOutSlot) = Counted(movedOut);
        box.SetLayout(newLayout);
        box.Destroy();
        Expect(!LetGo(movedOutSlot), "a layout replaced in its group box outlives the group box");

        Editor.DeclareProperty("level", "Level", ValueKind.Number, 0, 10, [2], value =>
        {
            if (value[0] == 7)
            {
                throw new InvalidOperationException("seven is refused");
            }
            value[0] = 3;
        });
        var info = (ModuleInfo*)NativeMemory.AllocZeroed((nuint)sizeof(ModuleInfo));
        Editor.Describe(info, "test", "1.0", [], []);
        var declared = info->Properties[0];
        Expect(info->PropertyCount == 1 && info->PropertySize == sizeof(NativeProperty), "the property is described");
        Expect(Utf8.Read(declared.Name) == "level" && declared.Kind == (uint)ValueKind.Number && declared.Minimum == 0 &&
               declared.Maximum == 10 && declared.Initial[0] == 2, "with its name, kind, range and initial value");
        double written = 3.4;
        Expect(declared.Write(declared.User, &written, 1, &Fake.Collect, IntPtr.Zero) == 0 && written == 3,
               "its write function gives back the value it keeps");
        written = 7;
        Expect(declared.Write(declared.User, &written, 1, &Fake.Collect, IntPtr.Zero) != 0 &&
               Fake.Error == "seven is refused", "an exception of the write function is a failure, with its message");
        Expect(Editor.SetProperty("level", 5) && Fake.ToldName == "level" && Fake.ToldValue == 5, "SetProperty tells the value");

        var sequence = new Sequence();
        var player = new Player();
        player.SetSequence(sequence);
        Expect(Fake.Numbers == (player.Handle, (uint)Property.Sequence, (double)sequence.Handle),
               "a player is given its sequence by its handle");
        double time = -1;
        var timed = player.TimeChanged.Connect(frames => time = frames);
        Fake.Call(Fake.Connections[timed], 12.5);
        Expect(time == 12.5, "TimeChanged gives the time in frames");

        var keys = new DopesheetView();
        keys.SetPlayer(player);
        Expect(Fake.Numbers == (keys.Handle, (uint)Property.Player, (double)player.Handle),
               "a dopesheet view is given its player by its handle");
        (string Json, bool Finished) change = ("", false);
        var edited = keys.KeysChanged.Connect(given => change = given);
        Fake.CallWithText(Fake.Connections[edited], "[]", true);
        Expect(change == ("[]", true), "KeysChanged gives the tracks and whether the change is done");

        var tree = new TreeView();
        (ulong Item, bool Expanded) unfolded = (0, false);
        var folded = tree.ItemExpanded.Connect(given => unfolded = given);
        Fake.CallWith(Fake.Connections[folded], new SignalData { Item = 9007199254740993UL, Boolean = 1 });
        Expect(unfolded == (9007199254740993UL, true), "ItemExpanded gives the item's id whole and whether unfolded");

        var table = new TableView();
        Expect(table.SetCell(7, 2, "x") && Fake.Cell == (7UL, 2u, "x"), "SetCell gives the row's id, the column and the text");
        Expect(!table.SetCell(7, -1, "y") && Fake.CellCalls == 1, "a negative column is refused before the editor");
        Expect(table.RemoveRows(3, 5) && Fake.Removed.SequenceEqual([3UL, 5UL]), "RemoveRows gives the ids");
        var cell = new CellEvent(0, 0, "");
        var typed = table.CellChanged.Connect(given => cell = given);
        using (var text = new Utf8("abc"))
        {
            Fake.CallWith(Fake.Connections[typed], new SignalData { Item = 42, Integer = 1, TextPointer = text.Pointer });
        }
        Expect(cell == new CellEvent(42, 1, "abc"), "CellChanged gives the row, the column and the text");
        (int Column, bool Descending) sort = (-2, false);
        var sorting = table.SortChanged.Connect(given => sort = given);
        Fake.CallWith(Fake.Connections[sorting], new SignalData { Integer = 3, Boolean = 1 });
        Expect(sort == (3, true), "SortChanged gives the column and whether from the highest");

        Console.WriteLine($"{failures} failure(s)");
        return failures == 0 ? 0 : 1;
    }
}
