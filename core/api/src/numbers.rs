//! The numbers of the interface objects, the same in every language (S1). Written by
//! `cargo xtask bindings` from `sdk/bindings.toml`: change them there.

numbered!(
    /// Kinds of interface objects, named as in Qt.
    Kind {
        /// a dock panel; obtained with panel(), never created
        Panel = 1,
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
        /// a modal window; created hidden, shown and hidden through VISIBLE
        Dialog = 22,
        /// curves edited by hand, drawn by the module curves
        CurveView = 23,
    }
);

numbered!(
    /// Properties. Texts go through set_text; everything else through set_numbers: a flag is 0 or
    /// 1, a colour is 0xRRGGBBAA, positions and sizes are in points.
    Property {
        Text = 1,
        ToolTip = 2,
        Enabled = 3,
        Visible = 4,
        Checked = 5,
        /// slider, spin box; kept within the range
        Value = 6,
        Minimum = 7,
        Maximum = 8,
        Step = 9,
        Decimals = 10,
        /// line edit, text
        Placeholder = 11,
        /// combo box
        CurrentIndex = 12,
        /// group box, text
        Title = 13,
        /// item: x, y in its parent
        Pos = 14,
        /// rectangle or ellipse item: x, y, width, height
        Rect = 15,
        /// line item: x1, y1, x2, y2
        Line = 16,
        PenColor = 17,
        PenWidth = 18,
        BrushColor = 19,
        /// rectangle item: corner radius
        Radius = 20,
        /// item: stacking order among its siblings
        ZValue = 21,
        /// item: 0 no, 1 along x, 2 along y, 3 both
        Movable = 22,
        Selectable = 23,
        Selected = 24,
        /// movable item: x, y, width, height its position stays in
        MoveBounds = 25,
        /// text item, 1 to 512, finite
        FontSize = 26,
        /// graphics view, paint area
        MinimumHeight = 27,
        /// graphics view: zoom
        ViewScale = 28,
        /// graphics view: x, y of the scene at its centre
        ViewCenter = 29,
        /// read only: entries of a combo box, children otherwise
        Count = 30,
        /// curve view, text: JSON [{label, colour: [r, g, b], visible, keys: [{time, value, mode,
        /// left, right}]}]; keys in time order, each at a time of its own, numbers within 1e9
        Curves = 31,
    }
);

numbered!(
    /// Signals, named as in Qt. Each tells what the user did, never a change the module made.
    Signal {
        /// push button
        Clicked = 1,
        /// check box: boolean
        Toggled = 2,
        /// slider, spin box: number
        ValueChanged = 3,
        /// slider: number
        SliderPressed = 4,
        /// slider: number; also after a click or a key
        SliderReleased = 5,
        /// line edit: text
        TextChanged = 6,
        /// line edit: text; spin box: number
        EditingFinished = 7,
        /// combo box: integer
        CurrentIndexChanged = 8,
        /// scene: item, x, y in the scene, button, modifiers
        ItemPressed = 9,
        /// scene: item dropped at x, y, moved by dx, dy
        ItemMoved = 10,
        /// scene: item, x, y
        ItemDoubleClicked = 11,
        /// scene; read each item's SELECTED
        SelectionChanged = 12,
        /// paint area: painter, width, height
        Paint = 13,
        /// paint area: x, y, button, modifiers
        MousePress = 14,
        /// paint area, while a button is held: x, y
        MouseMove = 15,
        /// paint area: x, y
        MouseRelease = 16,
        /// paint area: x, y, dx, dy
        Wheel = 17,
        /// dialog: the user closed it, which hid it
        Rejected = 18,
        /// curve view: text, the curves; boolean, whether the change is done
        CurvesChanged = 19,
    }
);

numbered!(
    /// Types of the values of animatable properties.
    PropertyKind {
        Number = 1,
        /// three numbers, such as a position
        Vector = 2,
        /// red, green and blue, from 0 to 1
        Colour = 3,
        /// one number, 0 or 1
        Boolean = 4,
    }
);
