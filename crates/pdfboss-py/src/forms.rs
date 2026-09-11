//! The interactive form as Python sees it: the form dictionary, its field
//! tree and each field's widgets, as frozen classes over the core form
//! types. Enumerations are kebab-case strings, object references `(num,
//! gen)` tuples, and field values plain Python data through the same
//! conversion `Element.value()` uses.

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};

use pdfboss_core::{
    AppearanceCharacteristics as CoreCharacteristics, ButtonKind, CaptionPosition,
    ChoiceOption as CoreChoiceOption, FieldFlags as CoreFieldFlags, FieldType,
    FormField as CoreFormField, InteractiveForm as CoreInteractiveForm, Quadding,
    Signature as CoreSignature, Widget as CoreWidget,
};

use crate::{dict_to_py, object_to_py, ref_tuple, repr_bool, repr_opt_str, repr_str};

fn field_type_str(field_type: FieldType) -> &'static str {
    match field_type {
        FieldType::Button => "button",
        FieldType::Text => "text",
        FieldType::Choice => "choice",
        FieldType::Signature => "signature",
    }
}

fn button_kind_str(kind: ButtonKind) -> &'static str {
    match kind {
        ButtonKind::PushButton => "push-button",
        ButtonKind::CheckBox => "check-box",
        ButtonKind::RadioButtons => "radio-buttons",
    }
}

fn quadding_str(quadding: Quadding) -> &'static str {
    match quadding {
        Quadding::Left => "left",
        Quadding::Centered => "centered",
        Quadding::Right => "right",
    }
}

fn caption_position_str(position: CaptionPosition) -> &'static str {
    match position {
        CaptionPosition::CaptionOnly => "caption-only",
        CaptionPosition::IconOnly => "icon-only",
        CaptionPosition::Below => "below",
        CaptionPosition::Above => "above",
        CaptionPosition::Right => "right",
        CaptionPosition::Left => "left",
        CaptionPosition::Overlaid => "overlaid",
    }
}

/// The document's interactive form dictionary: the root fields and the
/// defaults their widgets are drawn with.
#[pyclass(frozen)]
pub(crate) struct InteractiveForm {
    inner: CoreInteractiveForm,
}

impl From<CoreInteractiveForm> for InteractiveForm {
    fn from(inner: CoreInteractiveForm) -> InteractiveForm {
        InteractiveForm { inner }
    }
}

#[pymethods]
impl InteractiveForm {
    /// The root fields, those with no parent, as `(num, gen)` references.
    #[getter]
    fn fields(&self) -> Vec<(u32, u16)> {
        self.inner.fields.iter().copied().map(ref_tuple).collect()
    }

    /// Whether a reader should build the widgets' appearance streams
    /// itself.
    #[getter]
    fn need_appearances(&self) -> bool {
        self.inner.need_appearances
    }

    /// Whether the document carries at least one signature field with a
    /// signature.
    #[getter]
    fn signatures_exist(&self) -> bool {
        self.inner.signature_flags.signatures_exist
    }

    /// Whether the document may only be saved as incremental updates, so
    /// its signatures stay valid.
    #[getter]
    fn append_only(&self) -> bool {
        self.inner.signature_flags.append_only
    }

    /// The fields with calculation actions, in the order their values are
    /// recalculated, as `(num, gen)` references.
    #[getter]
    fn calculation_order(&self) -> Vec<(u32, u16)> {
        self.inner
            .calculation_order
            .iter()
            .copied()
            .map(ref_tuple)
            .collect()
    }

    /// The default resources for the fields' appearance streams, as a
    /// plain dict; `None` when the form names none.
    #[getter]
    fn default_resources<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        self.inner
            .default_resources
            .as_ref()
            .map(|dict| dict_to_py(py, dict))
            .transpose()
    }

    /// The document-wide default appearance string for variable text, a
    /// content fragment such as `/Helv 0 Tf 0 g`, as written.
    #[getter]
    fn default_appearance(&self) -> Option<&str> {
        self.inner.default_appearance.as_deref()
    }

    /// The document-wide default justification of variable text:
    /// `"left"`, `"centered"` or `"right"`; `None` when the form sets none.
    #[getter]
    fn quadding(&self) -> Option<&'static str> {
        self.inner.quadding.map(quadding_str)
    }

    /// Whether the form carries an XFA resource.
    #[getter]
    fn xfa(&self) -> bool {
        self.inner.xfa
    }

    fn __repr__(&self) -> String {
        format!(
            "InteractiveForm(fields={}, need_appearances={}, xfa={})",
            self.inner.fields.len(),
            repr_bool(self.inner.need_appearances),
            repr_bool(self.inner.xfa),
        )
    }
}

/// A field's flag word with one boolean per flag the standard defines;
/// which flags mean anything depends on the field's type.
#[pyclass(frozen)]
pub(crate) struct FieldFlags {
    inner: CoreFieldFlags,
}

impl FieldFlags {
    fn set_names(&self) -> Vec<&'static str> {
        let flags = self.inner;
        let table: [(&'static str, bool); 19] = [
            ("read_only", flags.read_only()),
            ("required", flags.required()),
            ("no_export", flags.no_export()),
            ("multiline", flags.multiline()),
            ("password", flags.password()),
            ("file_select", flags.file_select()),
            ("do_not_spell_check", flags.do_not_spell_check()),
            ("do_not_scroll", flags.do_not_scroll()),
            ("comb", flags.comb()),
            ("rich_text", flags.rich_text()),
            ("combo", flags.combo()),
            ("edit", flags.edit()),
            ("sort", flags.sort()),
            ("multi_select", flags.multi_select()),
            ("commit_on_sel_change", flags.commit_on_sel_change()),
            ("no_toggle_to_off", flags.no_toggle_to_off()),
            ("radio", flags.radio()),
            ("pushbutton", flags.pushbutton()),
            ("radios_in_unison", flags.radios_in_unison()),
        ];
        table
            .into_iter()
            .filter_map(|(name, set)| set.then_some(name))
            .collect()
    }
}

#[pymethods]
impl FieldFlags {
    /// The raw flag word.
    #[getter]
    fn bits(&self) -> u32 {
        self.inner.0
    }

    /// The names of the flags that are set, in bit order, each also
    /// available as a boolean attribute of the same name.
    #[getter]
    fn names(&self) -> Vec<&'static str> {
        self.set_names()
    }

    #[getter]
    fn read_only(&self) -> bool {
        self.inner.read_only()
    }

    #[getter]
    fn required(&self) -> bool {
        self.inner.required()
    }

    #[getter]
    fn no_export(&self) -> bool {
        self.inner.no_export()
    }

    #[getter]
    fn multiline(&self) -> bool {
        self.inner.multiline()
    }

    #[getter]
    fn password(&self) -> bool {
        self.inner.password()
    }

    #[getter]
    fn file_select(&self) -> bool {
        self.inner.file_select()
    }

    #[getter]
    fn do_not_spell_check(&self) -> bool {
        self.inner.do_not_spell_check()
    }

    #[getter]
    fn do_not_scroll(&self) -> bool {
        self.inner.do_not_scroll()
    }

    #[getter]
    fn comb(&self) -> bool {
        self.inner.comb()
    }

    #[getter]
    fn rich_text(&self) -> bool {
        self.inner.rich_text()
    }

    #[getter]
    fn combo(&self) -> bool {
        self.inner.combo()
    }

    #[getter]
    fn edit(&self) -> bool {
        self.inner.edit()
    }

    #[getter]
    fn sort(&self) -> bool {
        self.inner.sort()
    }

    #[getter]
    fn multi_select(&self) -> bool {
        self.inner.multi_select()
    }

    #[getter]
    fn commit_on_sel_change(&self) -> bool {
        self.inner.commit_on_sel_change()
    }

    #[getter]
    fn no_toggle_to_off(&self) -> bool {
        self.inner.no_toggle_to_off()
    }

    #[getter]
    fn radio(&self) -> bool {
        self.inner.radio()
    }

    #[getter]
    fn pushbutton(&self) -> bool {
        self.inner.pushbutton()
    }

    #[getter]
    fn radios_in_unison(&self) -> bool {
        self.inner.radios_in_unison()
    }

    fn __int__(&self) -> u32 {
        self.inner.0
    }

    fn __repr__(&self) -> String {
        format!("FieldFlags({})", self.set_names().join(", "))
    }
}

/// The captions and icons a button widget is drawn with.
#[pyclass(frozen)]
pub(crate) struct AppearanceCharacteristics {
    inner: CoreCharacteristics,
}

#[pymethods]
impl AppearanceCharacteristics {
    /// The caption shown while the button is at rest.
    #[getter]
    fn caption(&self) -> Option<&str> {
        self.inner.caption.as_deref()
    }

    /// The caption shown while the cursor is over the button.
    #[getter]
    fn rollover_caption(&self) -> Option<&str> {
        self.inner.rollover_caption.as_deref()
    }

    /// The caption shown while the mouse button is down.
    #[getter]
    fn alternate_caption(&self) -> Option<&str> {
        self.inner.alternate_caption.as_deref()
    }

    /// The normal icon, a form XObject as a `(num, gen)` reference.
    #[getter]
    fn icon(&self) -> Option<(u32, u16)> {
        self.inner.icon.map(ref_tuple)
    }

    /// The rollover icon as a `(num, gen)` reference.
    #[getter]
    fn rollover_icon(&self) -> Option<(u32, u16)> {
        self.inner.rollover_icon.map(ref_tuple)
    }

    /// The alternate icon as a `(num, gen)` reference.
    #[getter]
    fn alternate_icon(&self) -> Option<(u32, u16)> {
        self.inner.alternate_icon.map(ref_tuple)
    }

    /// Where the caption sits relative to the icon: `"caption-only"`,
    /// `"icon-only"`, `"below"`, `"above"`, `"right"`, `"left"` or
    /// `"overlaid"`.
    #[getter]
    fn caption_position(&self) -> &'static str {
        caption_position_str(self.inner.caption_position)
    }

    fn __repr__(&self) -> String {
        format!(
            "AppearanceCharacteristics(caption={}, caption_position={})",
            repr_opt_str(self.inner.caption.as_deref()),
            repr_str(self.caption_position())
        )
    }
}

/// A widget annotation that draws a field, as far as the field's state
/// needs it.
#[pyclass(frozen)]
pub(crate) struct Widget {
    inner: CoreWidget,
}

#[pymethods]
impl Widget {
    /// The annotation dictionary's `(num, gen)` reference; the field's own
    /// for a field merged with its single widget.
    #[getter]
    #[pyo3(name = "ref")]
    fn object_ref(&self) -> (u32, u16) {
        ref_tuple(self.inner.object)
    }

    /// The appearance state the widget shows.
    #[getter]
    fn appearance_state(&self) -> Option<&str> {
        self.inner.appearance_state.as_deref()
    }

    /// The widget's on state: the one key of its normal appearance
    /// dictionary other than `Off`. `None` when the normal appearance is a
    /// single stream or names no or several other states.
    #[getter]
    fn on_state(&self) -> Option<&str> {
        self.inner.on_state.as_deref()
    }

    /// The captions and icons a button widget is drawn with; `None`
    /// without the dictionary.
    #[getter]
    fn characteristics(&self) -> Option<AppearanceCharacteristics> {
        self.inner
            .characteristics
            .clone()
            .map(|inner| AppearanceCharacteristics { inner })
    }

    fn __repr__(&self) -> String {
        format!(
            "Widget(ref={:?}, appearance_state={}, on_state={})",
            self.object_ref(),
            repr_opt_str(self.inner.appearance_state.as_deref()),
            repr_opt_str(self.inner.on_state.as_deref())
        )
    }
}

/// One option of a choice field, or one export value of a check box or
/// radio button.
#[pyclass(frozen)]
pub(crate) struct ChoiceOption {
    inner: CoreChoiceOption,
}

#[pymethods]
impl ChoiceOption {
    /// The value exported for the option.
    #[getter]
    fn export_value(&self) -> &str {
        &self.inner.export_value
    }

    /// The text shown to the user; also what the field's value names when
    /// the option is selected.
    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    fn __repr__(&self) -> String {
        format!(
            "ChoiceOption(export_value={}, name={})",
            repr_str(&self.inner.export_value),
            repr_str(&self.inner.name)
        )
    }
}

/// A signature dictionary as the value of a signature field, read as
/// data: nothing here is verified.
#[pyclass(frozen)]
pub(crate) struct Signature {
    inner: CoreSignature,
}

#[pymethods]
impl Signature {
    /// The name of the preferred signature handler.
    #[getter]
    fn filter(&self) -> Option<&str> {
        self.inner.filter.as_deref()
    }

    /// The encoding of the signature value, such as `adbe.pkcs7.detached`.
    #[getter]
    fn sub_filter(&self) -> Option<&str> {
        self.inner.sub_filter.as_deref()
    }

    /// The `(offset, length)` pairs of the file bytes the signature
    /// covers.
    #[getter]
    fn byte_range(&self) -> Vec<(u64, u64)> {
        self.inner.byte_range.clone()
    }

    /// The signature value as stored, usually DER-encoded PKCS#7.
    #[getter]
    fn contents<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.inner.contents)
    }

    /// The name of the person or authority signing.
    #[getter]
    fn name(&self) -> Option<&str> {
        self.inner.name.as_deref()
    }

    /// The time of signing as the PDF date string written.
    #[getter]
    fn signing_time(&self) -> Option<&str> {
        self.inner.signing_time.as_deref()
    }

    /// The CPU host name or physical location of the signing.
    #[getter]
    fn location(&self) -> Option<&str> {
        self.inner.location.as_deref()
    }

    /// The reason for the signing.
    #[getter]
    fn reason(&self) -> Option<&str> {
        self.inner.reason.as_deref()
    }

    /// How to contact the signer to verify the signature.
    #[getter]
    fn contact_info(&self) -> Option<&str> {
        self.inner.contact_info.as_deref()
    }

    fn __repr__(&self) -> String {
        format!(
            "Signature(sub_filter={}, name={}, signing_time={})",
            repr_opt_str(self.inner.sub_filter.as_deref()),
            repr_opt_str(self.inner.name.as_deref()),
            repr_opt_str(self.inner.signing_time.as_deref())
        )
    }
}

/// One field of the interactive form with the inheritable entries taken
/// from the nearest ancestor that has them.
#[pyclass(frozen)]
pub(crate) struct FormField {
    inner: CoreFormField,
}

impl From<CoreFormField> for FormField {
    fn from(inner: CoreFormField) -> FormField {
        FormField { inner }
    }
}

#[pymethods]
impl FormField {
    /// The field dictionary's own `(num, gen)` reference.
    #[getter]
    #[pyo3(name = "ref")]
    fn object_ref(&self) -> (u32, u16) {
        ref_tuple(self.inner.object)
    }

    /// The parent field's `(num, gen)` reference; `None` for a root field.
    #[getter]
    fn parent(&self) -> Option<(u32, u16)> {
        self.inner.parent.map(ref_tuple)
    }

    /// The child fields as `(num, gen)` references, in the order written.
    #[getter]
    fn kids(&self) -> Vec<(u32, u16)> {
        self.inner.kids.iter().copied().map(ref_tuple).collect()
    }

    /// The widget annotations that draw this field.
    #[getter]
    fn widgets(&self) -> Vec<Widget> {
        self.inner
            .widgets
            .iter()
            .cloned()
            .map(|inner| Widget { inner })
            .collect()
    }

    /// `"button"`, `"text"`, `"choice"` or `"signature"`; `None` for a
    /// field that names no type, such as a container of other fields.
    #[getter]
    fn field_type(&self) -> Option<&'static str> {
        self.inner.field_type.map(field_type_str)
    }

    /// The field's own partial name.
    #[getter]
    fn partial_name(&self) -> Option<&str> {
        self.inner.partial_name.as_deref()
    }

    /// The fully qualified name: the partial names from the root field
    /// down, joined by periods.
    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    /// The name shown to the user in place of the field name.
    #[getter]
    fn alternate_name(&self) -> Option<&str> {
        self.inner.alternate_name.as_deref()
    }

    /// The name used when the field's data is exported.
    #[getter]
    fn mapping_name(&self) -> Option<&str> {
        self.inner.mapping_name.as_deref()
    }

    /// The field's flags.
    #[getter]
    fn flags(&self) -> FieldFlags {
        FieldFlags {
            inner: self.inner.flags,
        }
    }

    /// The field's value as plain Python data, in the format of the
    /// field's type; `None` without one. `text`, `state`, `checked`,
    /// `selected` and `signature` read it by type.
    #[getter]
    fn value<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        self.inner
            .value
            .as_ref()
            .map(|value| object_to_py(py, value))
            .transpose()
    }

    /// The value a reset-form action restores, as plain Python data.
    #[getter]
    fn default_value<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        self.inner
            .default_value
            .as_ref()
            .map(|value| object_to_py(py, value))
            .transpose()
    }

    /// The most characters a text field's text may hold.
    #[getter]
    fn max_len(&self) -> Option<u32> {
        self.inner.max_len
    }

    /// The options of a choice field, or the export values of a check box
    /// or radio button, one per widget.
    #[getter]
    fn options(&self) -> Vec<ChoiceOption> {
        self.inner
            .options
            .iter()
            .cloned()
            .map(|inner| ChoiceOption { inner })
            .collect()
    }

    /// The index into `options` of the first option a scrollable list box
    /// shows.
    #[getter]
    fn top_index(&self) -> u32 {
        self.inner.top_index
    }

    /// The indices into `options` of a multi-select choice field's
    /// selected options, as written.
    #[getter]
    fn selected_indices(&self) -> Vec<u32> {
        self.inner.selected_indices.clone()
    }

    /// The field's additional-actions dictionary as a plain dict, as
    /// written; `None` without one.
    #[getter]
    fn additional_actions<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        self.inner
            .additional_actions
            .as_ref()
            .map(|dict| dict_to_py(py, dict))
            .transpose()
    }

    /// The signature field lock dictionary as a `(num, gen)` reference.
    #[getter]
    fn lock(&self) -> Option<(u32, u16)> {
        self.inner.lock.map(ref_tuple)
    }

    /// The seed value dictionary as a `(num, gen)` reference.
    #[getter]
    fn seed_value(&self) -> Option<(u32, u16)> {
        self.inner.seed_value.map(ref_tuple)
    }

    /// The text of a text field; `None` for another type or no value.
    #[getter]
    fn text(&self) -> Option<String> {
        self.inner.text()
    }

    /// Which kind of button a button field is: `"push-button"`,
    /// `"check-box"` or `"radio-buttons"`; `None` for another type.
    #[getter]
    fn button_kind(&self) -> Option<&'static str> {
        self.inner.button_kind().map(button_kind_str)
    }

    /// The appearance state a check box or radio button field is in, the
    /// name its widgets key their on and off appearances by; `"Off"`
    /// without a value. `None` for a push button or another type.
    #[getter]
    fn state(&self) -> Option<&str> {
        self.inner.state()
    }

    /// Whether a check box is checked; `None` for a field that is no check
    /// box.
    #[getter]
    fn checked(&self) -> Option<bool> {
        self.inner.checked()
    }

    /// The indices into `widgets` of the widgets in the on state; the same
    /// indices pick the export values out of `options`. Empty for a field
    /// in the off state, a push button, or another type.
    #[getter]
    fn on_widgets(&self) -> Vec<usize> {
        self.inner.on_widgets()
    }

    /// The signature a signature field holds; `None` for another type or
    /// an unsigned field.
    #[getter]
    fn signature(&self) -> Option<Signature> {
        self.inner.signature().map(|inner| Signature { inner })
    }

    /// The names of a choice field's selected options; empty for another
    /// type or no value.
    #[getter]
    fn selected(&self) -> Vec<String> {
        self.inner.selected()
    }

    fn __repr__(&self) -> String {
        format!(
            "FormField(name={}, field_type={}, widgets={})",
            repr_str(&self.inner.name),
            repr_opt_str(self.field_type()),
            self.inner.widgets.len()
        )
    }
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<InteractiveForm>()?;
    module.add_class::<FormField>()?;
    module.add_class::<FieldFlags>()?;
    module.add_class::<Widget>()?;
    module.add_class::<AppearanceCharacteristics>()?;
    module.add_class::<ChoiceOption>()?;
    module.add_class::<Signature>()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{button_kind_str, caption_position_str, field_type_str, quadding_str};
    use pdfboss_core::{ButtonKind, CaptionPosition, FieldType, Quadding};

    #[test]
    fn enumerations_map_to_kebab_case_names() {
        assert_eq!(field_type_str(FieldType::Signature), "signature");
        assert_eq!(button_kind_str(ButtonKind::RadioButtons), "radio-buttons");
        assert_eq!(quadding_str(Quadding::Centered), "centered");
        assert_eq!(
            caption_position_str(CaptionPosition::CaptionOnly),
            "caption-only"
        );
    }

    #[test]
    fn pyclasses_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<super::InteractiveForm>();
        assert_send_sync::<super::FormField>();
        assert_send_sync::<super::FieldFlags>();
        assert_send_sync::<super::Widget>();
        assert_send_sync::<super::AppearanceCharacteristics>();
        assert_send_sync::<super::ChoiceOption>();
        assert_send_sync::<super::Signature>();
    }
}
