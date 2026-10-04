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

    /// <summary>Calls a slot as the editor would, even after its disconnect.</summary>
    public static void Call((IntPtr Slot, IntPtr User) kept)
    {
        var data = new SignalData();
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
        api->Connect = &Fake.Connect;
        api->Disconnect = &Fake.Disconnect;
        api->Log = &Fake.Log;
        api->SetProperty = &Fake.SetProperty;
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

        Console.WriteLine($"{failures} failure(s)");
        return failures == 0 ? 0 : 1;
    }
}
