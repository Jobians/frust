// The `frust-native-widgets` plugin's ONE Kotlin factory — the entire Android
// class surface this plugin will ever have, however many controls it grows.
//
// Every method immediately hands off to a Rust JNI export
// (`plugins/native-widgets/src/android/mod.rs`), which builds, mutates and
// tears down the real `android.widget` views itself; which *control* a slot
// means travels in `paramsJson` under the runtime's reserved
// `__frustControl`/`__frustSlot` keys, so a new control never needs a new
// class here (the "no per-control Kotlin, ever" rule).
//
// Package `dev.frust.nativewidgets` is baked into the mangled JNI symbol names
// on the Rust side (`Java_dev_frust_nativewidgets_FrustNativeControlFactory_*`)
// and is therefore **fixed once shipped** — `docs/CODE_STANDARDS.md`'s
// JNI-export-names-are-LAW rule. The fully-qualified name
// `dev.frust.nativewidgets.FrustNativeControlFactory` is also the platform-view
// `viewType` string the api layer publishes (`crate::api::builders`' Android
// `VIEW_TYPE`), which the embedding's `FrustViewHost` resolves reflectively
// through the application classloader — the subpackage keeps the `dev.frust.`
// prefix that host requires while staying OUT of the embedding module's
// exclusive bare `dev.frust` package.
//
// This file ships inside the plugin's own `com.android.library` module
// (`plugins/native-widgets/platform/android`), wired into a consuming app by
// `Contribution::GradleModule` — never copied into an app's source tree.
package dev.frust.nativewidgets

import android.app.Activity
import android.content.Context
import android.content.res.ColorStateList
import android.graphics.Color
import android.graphics.drawable.GradientDrawable
import android.util.Log
import android.view.View
import android.widget.LinearLayout
import com.google.android.material.button.MaterialButton
import com.google.android.material.button.MaterialButtonToggleGroup
import com.google.android.material.color.MaterialColors
import dev.frust.FrustPlatformViewFactory

class FrustNativeControlFactory : FrustPlatformViewFactory {
    /**
     * Build the control this slot's params name. On success Rust returns the
     * freshly created `View`; on failure (the params carry no known control
     * kind, construction failed, a re-entrant runtime call, an unreadable
     * paramsJson, or a caught Rust panic) Rust throws instead of returning
     * null — `FrustViewHost`'s `catch (Throwable)` logs that and marks the
     * slot dead rather than crashing its frame loop.
     */
    override fun createView(activity: Activity, context: Context, paramsJson: String): View =
        nativeCreateControl(paramsJson, activity, context)

    /**
     * A params change for an already-created control. Rust diffs the decoded
     * props against the ones it last applied and does nothing at all when they
     * are equal, so calling this every frame is free.
     */
    override fun updateParams(view: View, paramsJson: String) {
        nativeUpdateParams(view, paramsJson)
    }

    /**
     * The slot is gone: Rust releases every global reference the control
     * retained (the leak bar — ART aborts the process at 51,200 live global
     * refs). Resolution is by object identity, so a late dispose naming a
     * view that was already replaced is a silent no-op.
     */
    override fun disposeView(view: View) {
        nativeDisposeControl(view)
    }

    /**
     * The segmented control's Material half. Rust (`controls/segmented.rs`)
     * drives these `@JvmStatic` helpers over JNI — plain statics, not
     * `external` functions, so the frozen export set is unchanged — because
     * `MaterialButtonToggleGroup`'s listener is a Material interface that
     * cannot be built from raw JNI and must not be a supertype of
     * [FrustNativeListener]. Every programmatic mutation runs *muted*
     * ([SegmentedHolder.muted]): the group notifies its listener for
     * `check`/`clearChecked` too, and the app's own write-back must never
     * report itself as a user selection (the echo guard).
     */
    companion object {
        private const val TAG = "FrustNativeWidgets"

        // The button style wire values — `buttonStyle` in the button's params,
        // mirrored from `controls/button.rs`'s `ButtonStyle::wire` (append-only;
        // `tests/kotlin_conformance.rs` pins the pairing). An unknown value is
        // read as plain.
        private const val BUTTON_STYLE_PLAIN = 0
        private const val BUTTON_STYLE_PROMINENT = 1
        private const val BUTTON_STYLE_DESTRUCTIVE = 2
        private const val BUTTON_STYLE_GLASS = 3
        private const val BUTTON_STYLE_PROMINENT_GLASS = 4

        /**
         * The theme attribute whose style gives [style] its Material look:
         * the text button for plain, the tonal button for glass, the filled
         * button for the rest (destructive is filled in the error colour,
         * applied by [styleExtras]).
         */
        private fun buttonStyleAttr(style: Int): Int = when (style) {
            BUTTON_STYLE_GLASS -> com.google.android.material.R.attr.materialButtonTonalStyle
            BUTTON_STYLE_PROMINENT,
            BUTTON_STYLE_DESTRUCTIVE,
            BUTTON_STYLE_PROMINENT_GLASS -> com.google.android.material.R.attr.materialButtonStyle
            else -> androidx.appcompat.R.attr.borderlessButtonStyle
        }

        /**
         * What a style adds on top of its attribute style: the error fill for
         * destructive. A theme fill on a plain button is not drawn over the
         * text button's background but put into it
         * ([buttonSetThemedBackground]), so plain needs nothing here.
         */
        private fun styleExtras(button: MaterialButton, style: Int) {
            if (style != BUTTON_STYLE_DESTRUCTIVE) return
            val context = button.context
            val error = MaterialColors.getColor(context, androidx.appcompat.R.attr.colorError, 0xFFB3261E.toInt())
            val onError = MaterialColors.getColor(context, com.google.android.material.R.attr.colorOnError, 0xFFFFFFFF.toInt())
            val disabled = intArrayOf(-android.R.attr.state_enabled)
            val resting = button.backgroundTintList
            val disabledFill = resting?.getColorForState(disabled, error) ?: error
            button.backgroundTintList = ColorStateList(
                arrayOf(disabled, intArrayOf()),
                intArrayOf(disabledFill, error),
            )
            button.setTextColor(onError)
        }

        /**
         * The native button, in [style]. A `MaterialButton` under a Material3
         * app theme; under any other theme the Material inflation throws, and
         * the slot gets the framework `Button` the plugin always built (a
         * later [buttonSetStyle] on it is a no-op).
         */
        @JvmStatic
        fun buttonCreate(activity: Activity, qualified: Context, style: Int): View {
            try {
                val button = MaterialButton(
                    FrustMaterialContext.wrap(activity, qualified),
                    null,
                    buttonStyleAttr(style),
                )
                styleExtras(button, style)
                return button
            } catch (t: Throwable) {
                Log.w(TAG, "native_button needs a Material3 app theme for its styles; using a plain Button", t)
                return android.widget.Button(qualified)
            }
        }

        /**
         * Restyle a live button in place (a style change is an update, never
         * a remount). Everything a style owns is read off a reference button
         * built from its attribute style and copied across: background tint,
         * shape, insets, stroke, ripple and caption colours. That also drops
         * any theme fill, corner radius or caption colour the previous props
         * applied, so the button looks exactly like a fresh one in [style];
         * Rust re-applies whatever theme values the new props carry
         * afterwards. A framework `Button` (no Material theme) has no styles,
         * but still drops the theme look back to a fresh `Button`'s.
         */
        @JvmStatic
        fun buttonSetStyle(view: View, style: Int) {
            val button = view as? MaterialButton
            if (button == null) {
                val plain = view as? android.widget.Button ?: return
                val reference = android.widget.Button(plain.context)
                plain.background = reference.background
                plain.setTextColor(reference.textColors)
                return
            }
            val reference = MaterialButton(button.context, null, buttonStyleAttr(style))
            button.backgroundTintList = reference.backgroundTintList
            button.shapeAppearanceModel = reference.shapeAppearanceModel
            button.insetTop = reference.insetTop
            button.insetBottom = reference.insetBottom
            button.strokeColor = reference.strokeColor
            button.strokeWidth = reference.strokeWidth
            button.rippleColor = reference.rippleColor
            button.setTextColor(reference.textColors)
            styleExtras(button, style)
        }

        /**
         * A theme fill with a corner radius ([radiusPx], device pixels) — the
         * button's half of Rust's `Setter::ThemedBackground`. A
         * `MaterialButton` gets them in its own background (tint and shape,
         * with the insets dropped so the fill covers the whole view): a
         * replaced background is never handed back to the button, and
         * [buttonSetStyle] could then only tint the leftover drawable. The
         * corner goes through the shape model, not `cornerRadius`, which
         * skips a radius equal to the last one it set even after the shape
         * was reset. A framework `Button` gets a `GradientDrawable`.
         */
        @JvmStatic
        fun buttonSetThemedBackground(view: View, fill: Int, radiusPx: Float) {
            val button = view as? MaterialButton
            if (button == null) {
                view.background = GradientDrawable().apply {
                    setColor(fill)
                    cornerRadius = radiusPx
                }
                return
            }
            button.backgroundTintList = ColorStateList.valueOf(fill)
            button.shapeAppearanceModel = button.shapeAppearanceModel.withCornerSize(radiusPx)
            button.insetTop = 0
            button.insetBottom = 0
        }

        /**
         * A single-selection, selection-required toggle group wired to a
         * [FrustNativeListener] for [slotId]. Under a non-Material theme the
         * `MaterialButton` inflation throws; that is logged and rethrown so
         * the slot takes the host's existing dead-slot path rather than
         * crashing.
         */
        @JvmStatic
        fun segmentedCreate(activity: Activity, qualified: Context, slotId: Long): View {
            try {
                val group = MaterialButtonToggleGroup(FrustMaterialContext.wrap(activity, qualified))
                group.isSingleSelection = true
                group.isSelectionRequired = true
                val holder = SegmentedHolder(FrustNativeListener(slotId))
                group.tag = holder
                group.addOnButtonCheckedListener { g, checkedId, isChecked ->
                    if (holder.muting > 0 || !isChecked) return@addOnButtonCheckedListener
                    val index = g.indexOfChild(g.findViewById(checkedId))
                    if (index < 0) return@addOnButtonCheckedListener
                    holder.listener.reportSelection(index)
                    if (holder.momentary) g.post { holder.muted { g.clearChecked() } }
                }
                return group
            } catch (t: Throwable) {
                Log.w(TAG, "native_segmented needs a Material3 app theme; refusing the slot", t)
                throw t
            }
        }

        /** Drop every segment (the first half of a label-list replace). */
        @JvmStatic
        fun segmentedClear(group: View) {
            val g = group as MaterialButtonToggleGroup
            val holder = holder(g)
            holder.muted { g.removeAllViews() }
            holder.defaultTints.clear()
        }

        /** Append one outlined-style segment titled [label]. */
        @JvmStatic
        fun segmentedAdd(group: View, label: String) {
            val g = group as MaterialButtonToggleGroup
            val holder = holder(g)
            holder.muted {
                val button = MaterialButton(
                    g.context,
                    null,
                    com.google.android.material.R.attr.materialButtonOutlinedStyle,
                )
                button.id = View.generateViewId()
                button.text = label
                button.isEnabled = holder.enabled
                holder.defaultTints[button.id] = button.backgroundTintList
                holder.tint(button)
                // Equal shares of the group width; labels must fit a narrow slot.
                button.minWidth = 0
                button.minimumWidth = 0
                button.insetTop = 0
                button.insetBottom = 0
                val pad = (4 * g.resources.displayMetrics.density).toInt()
                button.setPadding(pad, button.paddingTop, pad, button.paddingBottom)
                button.maxLines = 1
                button.ellipsize = android.text.TextUtils.TruncateAt.END
                g.addView(button, LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.MATCH_PARENT, 1f))
            }
        }

        /**
         * Check segment [index], or clear the selection for a negative one.
         * A momentary group never shows a persistent check, so it clears
         * whatever the planned index is (the drawn state follows the props).
         */
        @JvmStatic
        fun segmentedSelect(group: View, index: Int) {
            val g = group as MaterialButtonToggleGroup
            val momentary = holder(g).momentary
            holder(g).muted {
                val child = if (index >= 0 && !momentary) g.getChildAt(index) else null
                if (child != null) g.check(child.id) else g.clearChecked()
            }
        }

        /**
         * Momentary: a tap reports then clears; the group no longer requires a
         * selection. Persistent (the default) is the reverse.
         */
        @JvmStatic
        fun segmentedSetMomentary(group: View, momentary: Boolean) {
            val g = group as MaterialButtonToggleGroup
            holder(g).momentary = momentary
            holder(g).muted {
                g.isSelectionRequired = !momentary
                if (momentary) g.clearChecked()
            }
        }

        /**
         * `View.setEnabled` on the group does not reach its buttons, so the
         * flag is applied to each segment (and remembered for later ones).
         */
        @JvmStatic
        fun segmentedSetEnabled(group: View, enabled: Boolean) {
            val g = group as MaterialButtonToggleGroup
            holder(g).enabled = enabled
            g.isEnabled = enabled
            for (i in 0 until g.childCount) g.getChildAt(i).isEnabled = enabled
        }

        /**
         * The checked segment's fill. `MaterialButton` has no single "selected
         * tint" property, so the checked state of each button's background
         * tint list is replaced; unchecked segments keep transparent, and
         * [hasTint] false restores each button's own list.
         */
        @JvmStatic
        fun segmentedSetTint(group: View, hasTint: Boolean, argb: Int) {
            val g = group as MaterialButtonToggleGroup
            val holder = holder(g)
            holder.tintArgb = if (hasTint) argb else null
            for (i in 0 until g.childCount) holder.tint(g.getChildAt(i) as MaterialButton)
        }

        /** Detach the listener (idempotent); the group dies with its slot. */
        @JvmStatic
        fun segmentedDispose(group: View) {
            val g = group as MaterialButtonToggleGroup
            g.clearOnButtonCheckedListeners()
            holder(g).listener.disarm()
        }

        private fun holder(group: View): SegmentedHolder = group.tag as SegmentedHolder
    }

    private external fun nativeCreateControl(
        paramsJson: String,
        activity: Activity,
        context: Context,
    ): View

    private external fun nativeUpdateParams(view: View, paramsJson: String)

    private external fun nativeDisposeControl(view: View)
}

/** Per-group state for the segmented control, kept in the group's `tag`. */
private class SegmentedHolder(val listener: FrustNativeListener) {
    /** > 0 while the app itself is mutating the group (echo guard). */
    var muting = 0
    var momentary = false
    var enabled = true
    var tintArgb: Int? = null

    /** Each segment's own background tint list, by view id, for tint-clear. */
    val defaultTints = HashMap<Int, ColorStateList?>()

    inline fun muted(block: () -> Unit) {
        muting++
        try {
            block()
        } finally {
            muting--
        }
    }

    fun tint(button: MaterialButton) {
        val argb = tintArgb
        button.backgroundTintList = if (argb == null) {
            defaultTints[button.id]
        } else {
            ColorStateList(
                arrayOf(intArrayOf(android.R.attr.state_checked), intArrayOf()),
                intArrayOf(argb, Color.TRANSPARENT),
            )
        }
    }
}
