// The `frust-native-widgets` plugin's Android app-chrome host: Material 3 bars
// (AppBarLayout + MaterialToolbar on top, a medium collapsing bar for a large
// title, BottomNavigationView at the bottom, the toolbar's floating action
// buttons) around the frust embedding's content root, driven from Rust's
// `ChromeHost` (`plugins/native-widgets/src/chrome/host.rs`) over JNI.
//
// FROZEN once shipped: the binary class name `dev.frust.nativewidgets.FrustNativeChrome`
// (Rust loads it through the application classloader), the `@JvmStatic` entry
// points `install`/`apply`/`dispose`/`setIcon`/`setScroll`, the JNI exports
// `Java_dev_frust_nativewidgets_FrustNativeChrome_nativeOnChromeEvent` and
// `..._nativeOnChromeLost`, the `INSTALL_*`/`ICON_SLOT_*` codes, the JSON keys
// below and [CHROME_MENU_STRIDE]/[CHROME_TAB_RESELECT_SHIFT] (twins of
// `src/events.rs`). Event kinds are `FrustNativeListener`'s `KIND_CHROME_*`.
//
// Flat JSON keys (`install` takes all of them, `apply` only the fields its
// `changed` array names — `ChromeField` spellings):
//   Tabs:     tabCount, tab<i>Title, tab<i>Enabled, tab<i>Badge (absent/null =
//             none, "" = dot), tab<i>IconRev (0 = no icon)
//   Selected: selectedIndex (-1 = none)
//   TopBar:   hasTop          Title: title          Subtitle: subtitle (absent/null = none)
//   Leading:  leading = "none" | "back" | "image" | "icon"   LargeTitle: largeTitle (ignored here)
//   Actions:  actionCount, action<i>Title, action<i>HasIcon, action<i>MenuCount,
//             action<i>Badge (absent/null = none, "" = dot), action<i>Prominent (absent = false),
//             menu<i>_<j>Title, menu<i>_<j>Destructive, menu<i>_<j>Enabled,
//             toolbarCount, toolbarPrimary (the last entry is the primary),
//             toolbar<i>Title, toolbar<i>HasIcon, toolbar<i>MenuCount, toolbar<i>Badge, toolbar<i>Prominent,
//             toolbarMenu<i>_<j>Title, toolbarMenu<i>_<j>Destructive, toolbarMenu<i>_<j>Enabled
//   Brightness: dark
//   ActionBadges (apply only, never beside Actions): action<i>Badge and toolbar<i>Badge
//             for every entry (null = none); the counts are unchanged
// largeTitle = true builds the medium collapsing top bar (the class doc's Large title);
// the table's "(ignored here)" is stale and stays only because a Rust test pins it verbatim.
// Toolbar entry i reports as action index actionCount + i (its menu likewise).
// "image" is a round avatar, "icon" a plain navigation icon; both read ICON_SLOT_LEADING.
// Icon bytes never travel as JSON: `setIcon(slot, index, png)` caches one
// bitmap per (slot, index) until `dispose`, before or after the call naming it.
package dev.frust.nativewidgets

import android.app.Activity
import android.content.Context
import android.content.res.Configuration
import android.content.res.ColorStateList
import android.content.res.Resources
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Rect
import android.graphics.drawable.BitmapDrawable
import android.os.Build
import android.os.Looper
import android.text.SpannableString
import android.text.Spanned
import android.text.style.ForegroundColorSpan
import android.util.Log
import android.view.ContextThemeWrapper
import android.view.Gravity
import android.view.Menu
import android.view.MenuItem
import android.view.View
import android.view.ViewGroup
import android.view.ViewTreeObserver
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.PopupMenu
import androidx.coordinatorlayout.widget.CoordinatorLayout
import androidx.core.graphics.Insets
import androidx.core.graphics.drawable.RoundedBitmapDrawableFactory
import androidx.core.view.DisplayCutoutCompat
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import com.google.android.material.appbar.AppBarLayout
import com.google.android.material.appbar.CollapsingToolbarLayout
import com.google.android.material.appbar.MaterialToolbar
import com.google.android.material.badge.BadgeDrawable
import com.google.android.material.badge.BadgeUtils
import com.google.android.material.badge.ExperimentalBadgeUtils
import com.google.android.material.bottomnavigation.BottomNavigationView
import com.google.android.material.color.MaterialColors
import com.google.android.material.floatingactionbutton.ExtendedFloatingActionButton
import com.google.android.material.floatingactionbutton.FloatingActionButton
import com.google.android.material.navigation.NavigationBarView
import dev.frust.FrustSurfaceView
import org.json.JSONObject
import kotlin.math.max
import kotlin.math.roundToInt

/**
 * Installs Material 3 app chrome around the frust content root of the host
 * Activity ([FrustNativePresenter.hostActivity]).
 *
 * ## Layout without re-parenting
 *
 * The embedding's root `FrameLayout` (which hosts the [FrustSurfaceView]) is
 * never detached: detaching would destroy the surface synchronously, re-entering
 * the frust runtime from inside the Rust call that is installing the chrome,
 * and show a black frame. Instead the bars join `android.R.id.content` as
 * siblings placed *before* the root (so a pre-API-30 sequential insets dispatch
 * reaches them with the real insets), and the root's padding shrinks the
 * surface to the space between them — synced from the bars' laid-out heights in
 * a pre-draw pass, which cancels that one draw instead of showing a stale one.
 *
 * ## Large title
 *
 * With the spec's large-title flag the top bar is Material 3's medium
 * collapsing bar: a [CoordinatorLayout] (only so the [AppBarLayout.Behavior]
 * can offset the bar) around the [AppBarLayout], whose
 * [CollapsingToolbarLayout] (`collapsingToolbarLayoutMediumStyle`, scroll |
 * exit-until-collapsed) draws the title and subtitle, expanded and collapsed,
 * over a pinned [MaterialToolbar] carrying the leading item and the actions.
 * The frust surface is no nested-scrolling child, so the bar never scrolls by
 * itself (dragging it is off): [setScroll]'s lifted signal collapses it, back
 * at the top it expands, animated once the bar is on screen. It does not
 * collapse when the body's [setScroll] `max` would not stay positive once the
 * body grows by the collapse range (offsets are logical px, taken as dp), so a
 * short page never bounces. The root's top padding follows the bar's visible
 * height — expanded, collapsed or in between — in the same pre-draw pass. A
 * flag change swaps the top bar alone in one main-thread turn ([swapTopBar]):
 * the old bar's views and listeners go with it, and the new one is laid out
 * before the next draw, so nothing flashes.
 *
 * ## Insets
 *
 * The app bar pads itself by the status-bar/cutout insets; the bottom bar keeps
 * Material's own padding. The root gets a copy with the top/bottom system-bar
 * and cutout insets zeroed on the sides a visible bar covers, and the IME inset
 * reduced by the bottom bar's height, so the surface reports only what it
 * touches. While the keyboard is visible the bottom navigation is hidden
 * (Material guidance): the surface then extends down to the keyboard and the
 * IME inset is forwarded whole. Nothing here consumes insets.
 *
 * ## Toolbar
 *
 * The spec's toolbar is a column of floating action buttons, one more sibling
 * added *after* the root so it draws over the surface: the secondary entries
 * as [ExtendedFloatingActionButton]s stacked above the primary, a
 * [FloatingActionButton] when it has an icon (else extended, with its title).
 * It floats over the body and takes no root padding. The root's insets
 * listener positions it [FAB_MARGIN_DP] above the bottom navigation while
 * that is shown, else above the larger of the IME and navigation-bar insets,
 * and the same distance in from the trailing edge's system insets.
 *
 * ## Badges and prominence
 *
 * An action's badge is a [BadgeDrawable] on its menu item ([BadgeUtils]), a
 * toolbar entry's one on its button; an ActionBadges apply updates them in
 * place without rebuilding the menu or the buttons. A prominent action is
 * tinted `colorPrimary` (a toolbar button is filled with it, its content
 * `colorOnPrimary`).
 *
 * ## Threading and ordering
 *
 * Every entry point and listener runs on the main thread (the frust UI
 * thread), in call order; nothing is posted. Taps report synchronously with
 * the generation [install] was given. [dispose] never calls into Rust.
 */
object FrustNativeChrome {
    /** [install] built the chrome. */
    const val INSTALL_OK = 0

    /** [install] declined (non-Material theme, malformed spec, a menu of [CHROME_MENU_STRIDE]+ items). */
    const val INSTALL_REFUSED = 1

    /** [install] found no live Activity whose content hosts the frust surface; retried later. */
    const val INSTALL_NO_ACTIVITY = 2

    /** A menu choice's `detail` is `action * CHROME_MENU_STRIDE + item`. */
    const val CHROME_MENU_STRIDE = 1000

    /** A tab report's reselect flag rides this bit of `detail`. */
    const val CHROME_TAB_RESELECT_SHIFT = 32

    /** Scroll offset (logical px) above which the app bar counts as lifted. */
    private const val LIFT_THRESHOLD_PX = 1f

    /** [setIcon] slot: a bottom tab's icon, `index` = tab index. */
    const val ICON_SLOT_TAB = 0

    /** [setIcon] slot: a top-bar action's icon, `index` = action index. */
    const val ICON_SLOT_ACTION = 1

    /** [setIcon] slot: the leading image (avatar) or icon, `index` ignored. */
    const val ICON_SLOT_LEADING = 2

    /** [setIcon] slot: a toolbar entry's icon, `index` = toolbar entry index. */
    const val ICON_SLOT_TOOLBAR = 3

    private const val TAG = "frust"
    private const val LEADING_NONE = "none"
    private const val LEADING_BACK = "back"
    private const val LEADING_IMAGE = "image"
    private const val LEADING_ICON = "icon"

    /** Material's toolbar action icon box (Material 3 spec: 24dp). */
    private const val ACTION_ICON_DP = 24

    /** The leading avatar's diameter (Material 3 top app bar avatar: 32dp). */
    private const val AVATAR_DP = 32

    /** A bottom navigation item's icon box (Material 3 spec: 24dp). */
    private const val TAB_ICON_DP = 24

    /** The toolbar column's distance from the bars and edges (Material 3 FAB margin: 16dp). */
    private const val FAB_MARGIN_DP = 16

    /** The gap between stacked toolbar buttons. */
    private const val FAB_GAP_DP = 12

    /** The medium collapsing bar's expanded height without a theme value (Material 3 spec: 112dp). */
    private const val MEDIUM_BAR_DP = 112

    /** The small bar's toolbar height without a theme value (Material 3 spec: 64dp). */
    private const val SMALL_BAR_DP = 64

    /** [setIcon] decodes to at most 4x the slot's on-screen size, but never caps below this (px). */
    private const val MIN_ICON_DECODE_PX = 256

    /** Consecutive [INSTALL_NO_ACTIVITY] answers after which [install] warns once. */
    private const val NO_ACTIVITY_WARN_AFTER = 30

    // The `changed` array's spellings: Rust's `ChromeField` names, declared once
    // here (`tests/kotlin_conformance.rs` pins them against `android_wire`).
    private const val FIELD_TABS = "Tabs"
    private const val FIELD_SELECTED = "Selected"
    private const val FIELD_TOP_BAR = "TopBar"
    private const val FIELD_TITLE = "Title"
    private const val FIELD_SUBTITLE = "Subtitle"
    private const val FIELD_LEADING = "Leading"
    private const val FIELD_ACTIONS = "Actions"
    private const val FIELD_LARGE_TITLE = "LargeTitle"
    private const val FIELD_SCROLL = "Scroll"
    private const val FIELD_BRIGHTNESS = "Brightness"

    /** Badges alone changed; an apply-only field, never beside [FIELD_ACTIONS]. */
    private const val FIELD_ACTION_BADGES = "ActionBadges"

    /** Every field [install]'s spec carries (Rust's `INSTALL_FIELDS`; [FIELD_SCROLL] rides [setScroll]). */
    private val INSTALL_FIELDS = setOf(
        FIELD_TABS,
        FIELD_SELECTED,
        FIELD_TOP_BAR,
        FIELD_TITLE,
        FIELD_SUBTITLE,
        FIELD_LEADING,
        FIELD_ACTIONS,
        FIELD_LARGE_TITLE,
        FIELD_BRIGHTNESS,
    )

    private class Tab(val title: String, val enabled: Boolean, val badge: String?, val iconRev: Long)

    private class MenuEntry(val title: String, val destructive: Boolean, val enabled: Boolean)

    private data class Action(
        val title: String,
        val hasIcon: Boolean,
        val menu: List<MenuEntry>,
        val badge: String? = null,
        val prominent: Boolean = false,
    )

    /** The retained spec, merged field by field by [apply]. */
    private class Model {
        var tabs: List<Tab> = emptyList()
        var selected = -1
        var hasTop = false
        var title = ""
        var subtitle: String? = null
        var leading = LEADING_NONE
        var actions: List<Action> = emptyList()

        /** The toolbar's entries, the primary last when [toolbarPrimary]. */
        var toolbar: List<Action> = emptyList()
        var toolbarPrimary = false
        var largeTitle = false
        var dark = false
    }

    /** The top app bar: the small one, or the medium collapsing one for a large title. */
    private class TopBar(
        /** The view [android.R.id.content] holds: [appBar] itself, or the CoordinatorLayout around it. */
        val outer: ViewGroup,
        val appBar: AppBarLayout,
        val toolbar: MaterialToolbar,
        /** The layout drawing the large title; `null` for the small bar. */
        val collapsing: CollapsingToolbarLayout?,
    ) {
        /** The badges on [toolbar]'s menu items, by item id. */
        val actionBadges = HashMap<Int, BadgeDrawable>()

        /** Whether [appBar] was last told to collapse (collapsing bar only). */
        var collapsed = false
    }

    private class Bars(
        val context: Context,
        var top: TopBar,
        val bottom: BottomNavigationView,
        /** The toolbar's floating action buttons, top to bottom. */
        val fabs: LinearLayout,
    ) {
        val appBar: AppBarLayout get() = top.appBar
        val toolbar: MaterialToolbar get() = top.toolbar

        /** The badges on the current [toolbar]'s menu items, by item id. */
        val actionBadges: HashMap<Int, BadgeDrawable> get() = top.actionBadges

        /** The badges on [fabs]' buttons, by toolbar entry index. */
        val fabBadges = HashMap<Int, BadgeDrawable>()
    }

    private class Installed(
        val generation: Long,
        val activity: Activity,
        val content: FrameLayout,
        val root: ViewGroup,
        val model: Model,
        var bars: Bars,
    ) {
        val basePadding = intArrayOf(root.paddingLeft, root.paddingTop, root.paddingRight, root.paddingBottom)
        val baseRootId = root.id
        var observer: ViewTreeObserver? = null

        /** The lifted state last handed to [AppBarLayout.setLifted]; a rebuilt bar re-adopts it. */
        var lifted = false

        /** The last [setScroll] `max`, which gates collapsing a large title. */
        var scrollMax = 0f

        /** The IME visibility last seen by the insets listener; the bottom bar is hidden while it is true. */
        var imeVisible = false
    }

    private var live: Installed? = null
    private val icons = HashMap<Long, Bitmap>()
    private var applyingSelection = false
    private var popup: PopupMenu? = null
    private var debuggable = false

    /** [INSTALL_NO_ACTIVITY] answers since the last [INSTALL_OK]. */
    private var noActivityStreak = 0

    /**
     * Build the chrome for `specJson` (every key) into the host Activity and
     * stamp every later report with `generation`. Returns an `INSTALL_*` code.
     * A chrome already installed is torn down first; cached icons are kept.
     */
    @JvmStatic
    fun install(generation: Long, specJson: String): Int {
        assertMainThread()
        live?.let {
            live = null
            detachQuietly(it)
        }
        val activity = FrustNativePresenter.hostActivity { frustRoot(it) != null }
        val content = activity?.let(::contentOf)
        val root = content?.let(::rootOf)
        if (activity == null || content == null || root == null) {
            Log.d(TAG, "Frust: native app chrome waits — no live Activity hosts the frust surface yet")
            noActivityStreak += 1
            if (noActivityStreak == NO_ACTIVITY_WARN_AFTER) {
                Log.w(
                    TAG,
                    "Frust: native app chrome found no Activity hosting the frust surface after " +
                        "$NO_ACTIVITY_WARN_AFTER attempts — the frust embedding's root is likely not the " +
                        "content view of any created Activity (e.g. wrapped in another root); still retrying",
                )
            }
            return INSTALL_NO_ACTIVITY
        }
        debuggable = (activity.applicationInfo.flags and android.content.pm.ApplicationInfo.FLAG_DEBUGGABLE) != 0
        val model = Model()
        try {
            mergeAll(model, JSONObject(specJson))
        } catch (t: Throwable) {
            Log.w(TAG, "Frust: native app chrome refused — malformed spec ($t)")
            return INSTALL_REFUSED
        }
        if ((model.actions + model.toolbar).any { it.menu.size >= CHROME_MENU_STRIDE }) {
            Log.w(TAG, "Frust: native app chrome refused — a menu has $CHROME_MENU_STRIDE or more items")
            return INSTALL_REFUSED
        }
        val bars = try {
            buildBars(activity, model.dark, model.largeTitle)
        } catch (t: Throwable) {
            warnTheme(t)
            return INSTALL_REFUSED
        }
        val installed = Installed(generation, activity, content, root, model, bars)
        live = installed
        try {
            attach(installed)
            applyAll(installed)
        } catch (t: Throwable) {
            live = null
            detachQuietly(installed)
            warnTheme(t)
            return INSTALL_REFUSED
        }
        noActivityStreak = 0
        return INSTALL_OK
    }

    /**
     * Merge the fields `diffJson`'s `changed` array names and update only
     * those. A view update that throws leaves the model ahead of the views, so
     * the whole merged model is re-applied once — through [rebuildBars] when
     * the diff changed Brightness, since only a rebuild inflates the bars for
     * the new `dark`; if that throws too, the bars stay as they are until the
     * next apply.
     */
    @JvmStatic
    fun apply(diffJson: String) {
        assertMainThread()
        val installed = live ?: return
        val (json, changed) = try {
            val parsed = JSONObject(diffJson)
            parsed to changedFields(parsed)
        } catch (t: Throwable) {
            Log.w(TAG, "Frust: native app chrome apply dropped — malformed diff", t)
            return
        }
        try {
            merge(installed.model, json, changed)
            applyChanged(installed, changed)
        } catch (t: Throwable) {
            val rebuild = FIELD_BRIGHTNESS in changed
            val fallback = if (rebuild) "rebuilding the bars" else "re-applying the whole spec"
            Log.w(TAG, "Frust: native app chrome apply failed; $fallback", t)
            try {
                if (rebuild) rebuildBars(installed) else applyAll(installed)
            } catch (again: Throwable) {
                Log.w(TAG, "Frust: native app chrome re-apply failed; the bars stay as they are", again)
            }
        }
    }

    /**
     * Remove the chrome and restore the content root. Idempotent and
     * panic-free; tolerates a destroyed Activity; never calls into Rust.
     */
    @JvmStatic
    fun dispose() {
        val installed = live
        live = null
        icons.clear()
        if (installed != null) detachQuietly(installed)
    }

    /**
     * Cache icon `png` for (`slot`, `index`) and refresh the live bar showing
     * it; empty bytes clear it. The decode is bounded ([decodeIcon]); bytes
     * that do not decode are logged and leave the entry empty.
     */
    @JvmStatic
    fun setIcon(slot: Int, index: Int, png: ByteArray) {
        assertMainThread()
        val key = iconKey(slot, index)
        val bitmap = if (png.isEmpty()) null else decodeIcon(png, iconDecodeCap(slot))
        if (bitmap == null && png.isNotEmpty()) {
            Log.w(TAG, "Frust: native app chrome icon (slot $slot, index $index) did not decode (${png.size} bytes); shown without it")
        }
        if (bitmap == null) icons.remove(key) else icons[key] = bitmap
        val installed = live ?: return
        try {
            when (slot) {
                ICON_SLOT_TAB -> refreshTabIcon(installed, index)
                ICON_SLOT_ACTION -> applyActions(installed)
                ICON_SLOT_LEADING -> applyLeading(installed)
                ICON_SLOT_TOOLBAR -> applyToolbar(installed)
            }
        } catch (t: Throwable) {
            Log.w(TAG, "Frust: native app chrome icon refresh failed", t)
        }
    }

    /**
     * Mirror the body's scroll position: the app bar lifts once scrolled.
     *
     * The lift source is the frust scroll offset (`ChromeSpec.scroll`), NOT a
     * RecyclerView or other NestedScrollingChild: the frust surface is none,
     * so Material's own lift-on-scroll never fires and the lifted state is
     * driven by hand. Android is the only consumer of `ChromeSpec.scroll`
     * (iOS draws a frust-side edge fade instead). `(0, 0)` means at the top.
     * The same lifted state collapses a large title ([applyCollapse]).
     * Main thread only; `setLifted` and `setExpanded` run only when their
     * state flips, so a scrolling frame costs a few compares.
     */
    @JvmStatic
    fun setScroll(offset: Float, max: Float) {
        assertMainThread()
        val installed = live ?: return
        installed.scrollMax = max
        val lifted = offset > LIFT_THRESHOLD_PX && max > 0f
        if (lifted != installed.lifted) {
            installed.lifted = lifted
            installed.bars.appBar.setLifted(lifted)
        }
        applyCollapse(installed)
    }

    /** [FrustNativePresenter]'s lifecycle hook: the host Activity is going away. */
    internal fun onActivityDestroyed(activity: Activity) {
        val installed = live ?: return
        if (installed.activity === activity) reportLost(installed)
    }

    @JvmStatic
    private external fun nativeOnChromeEvent(generation: Long, kind: Int, detail: Long)

    @JvmStatic
    private external fun nativeOnChromeLost(generation: Long)

    // --- spec parsing -------------------------------------------------------

    private fun mergeAll(model: Model, json: JSONObject) = merge(model, json, INSTALL_FIELDS)

    private fun changedFields(json: JSONObject): Set<String> {
        val array = json.optJSONArray("changed") ?: return emptySet()
        return (0 until array.length()).map { array.optString(it) }.toSet()
    }

    private fun merge(model: Model, json: JSONObject, fields: Set<String>) {
        if (FIELD_TABS in fields) {
            model.tabs = List(json.optInt("tabCount", 0)) { i ->
                Tab(
                    json.optString("tab${i}Title"),
                    json.optBoolean("tab${i}Enabled", true),
                    optStringOrNull(json, "tab${i}Badge"),
                    json.optLong("tab${i}IconRev", 0L),
                )
            }
        }
        if (FIELD_SELECTED in fields) model.selected = json.optInt("selectedIndex", -1)
        if (FIELD_TOP_BAR in fields) model.hasTop = json.optBoolean("hasTop", false)
        if (FIELD_TITLE in fields) model.title = json.optString("title")
        if (FIELD_SUBTITLE in fields) model.subtitle = optStringOrNull(json, "subtitle")
        if (FIELD_LEADING in fields) model.leading = json.optString("leading", LEADING_NONE)
        if (FIELD_ACTIONS in fields) {
            model.actions = List(json.optInt("actionCount", 0)) { a ->
                parseAction(json, "action", "menu", a)
            }
            model.toolbar = List(json.optInt("toolbarCount", 0)) { t ->
                parseAction(json, "toolbar", "toolbarMenu", t)
            }
            model.toolbarPrimary = json.optBoolean("toolbarPrimary", false)
        } else if (FIELD_ACTION_BADGES in fields) {
            model.actions = model.actions.mapIndexed { a, action ->
                action.copy(badge = optStringOrNull(json, "action${a}Badge"))
            }
            model.toolbar = model.toolbar.mapIndexed { t, entry ->
                entry.copy(badge = optStringOrNull(json, "toolbar${t}Badge"))
            }
        }
        if (FIELD_LARGE_TITLE in fields) model.largeTitle = json.optBoolean("largeTitle", false)
        if (FIELD_BRIGHTNESS in fields) model.dark = json.optBoolean("dark", false)
    }

    /** Action `a`'s keys under `prefix` and its menu's under `menuPrefix` (the header's two shapes). */
    private fun parseAction(json: JSONObject, prefix: String, menuPrefix: String, a: Int): Action {
        val count = json.optInt("$prefix${a}MenuCount", 0).coerceIn(0, CHROME_MENU_STRIDE - 1)
        return Action(
            json.optString("$prefix${a}Title"),
            json.optBoolean("$prefix${a}HasIcon", false),
            List(count) { m ->
                MenuEntry(
                    json.optString("$menuPrefix${a}_${m}Title"),
                    json.optBoolean("$menuPrefix${a}_${m}Destructive", false),
                    json.optBoolean("$menuPrefix${a}_${m}Enabled", true),
                )
            },
            optStringOrNull(json, "$prefix${a}Badge"),
            json.optBoolean("$prefix${a}Prominent", false),
        )
    }

    private fun optStringOrNull(json: JSONObject, key: String): String? =
        if (!json.has(key) || json.isNull(key)) null else json.optString(key)

    // --- building and attaching --------------------------------------------

    /**
     * The frust embedding's root in `activity`: the first child of its
     * `android.R.id.content` [FrameLayout], hosting a [FrustSurfaceView] —
     * `null` before `setContentView` or for a foreign Activity. Reads
     * through `peekDecorView`, so probing an Activity never installs its decor.
     */
    private fun frustRoot(activity: Activity): ViewGroup? = contentOf(activity)?.let(::rootOf)

    private fun contentOf(activity: Activity): FrameLayout? =
        activity.window?.peekDecorView()?.findViewById<View?>(android.R.id.content) as? FrameLayout

    private fun rootOf(content: FrameLayout): ViewGroup? {
        val root = (if (content.childCount > 0) content.getChildAt(0) else null) as? ViewGroup ?: return null
        return if ((0 until root.childCount).any { root.getChildAt(it) is FrustSurfaceView }) root else null
    }

    /**
     * Inflate the bars in the Activity's own theme with the night bit forced to
     * `dark` (the frust app's brightness, not the system's), the top bar the
     * collapsing one when `large`. Material's theme enforcement throws here
     * for a non-Material theme.
     */
    private fun buildBars(activity: Activity, dark: Boolean, large: Boolean): Bars {
        val config = Configuration(activity.resources.configuration)
        val night = if (dark) Configuration.UI_MODE_NIGHT_YES else Configuration.UI_MODE_NIGHT_NO
        config.uiMode = (config.uiMode and Configuration.UI_MODE_NIGHT_MASK.inv()) or night
        val theme = activity.packageManager.getActivityInfo(activity.componentName, 0).themeResource
        val context = ContextThemeWrapper(activity, theme)
        context.applyOverrideConfiguration(config)
        val top = buildTopBar(context, large)
        val bottom = BottomNavigationView(context)
        bottom.labelVisibilityMode = NavigationBarView.LABEL_VISIBILITY_LABELED
        val fabs = LinearLayout(context)
        fabs.orientation = LinearLayout.VERTICAL
        fabs.gravity = Gravity.END
        fabs.visibility = View.GONE
        return Bars(context, top, bottom, fabs)
    }

    /**
     * The small top bar (an [AppBarLayout] holding the [MaterialToolbar]),
     * or for `large` the medium collapsing one (class doc's *Large title*).
     */
    private fun buildTopBar(context: Context, large: Boolean): TopBar {
        val match = ViewGroup.LayoutParams.MATCH_PARENT
        val wrap = ViewGroup.LayoutParams.WRAP_CONTENT
        val appBar = AppBarLayout(context)
        appBar.fitsSystemWindows = false
        // Lift on scroll with no liftOnScrollTargetViewId: AppBarLayout has no
        // nested-scrolling sibling to watch, so [setScroll] drives setLifted.
        // setLiftable(true) pins the liftable state so layout passes cannot
        // recompute it away (Material 1.14.0 setLiftable sets liftableOverride,
        // which also turns the Behavior's own lift updates into no-ops).
        appBar.isLiftOnScroll = true
        appBar.setLiftable(true)
        val toolbar = MaterialToolbar(context)
        if (!large) {
            appBar.addView(toolbar, AppBarLayout.LayoutParams(match, wrap))
            return TopBar(appBar, appBar, toolbar, null)
        }
        val collapsing = CollapsingToolbarLayout(context, null, com.google.android.material.R.attr.collapsingToolbarLayoutMediumStyle)
        collapsing.fitsSystemWindows = false
        // CollapsingToolbarLayout consumes the insets it is handed, which a
        // pre-API-30 sequential dispatch would then withhold from the bottom
        // bar and the root; pass them on untouched (the app bar pads itself).
        ViewCompat.setOnApplyWindowInsetsListener(collapsing) { _, insets -> insets }
        // The toolbar's height must be fixed: the collapsing layout puts a
        // MATCH_PARENT placeholder view inside it, which grows a wrap_content
        // toolbar to the whole bar. The collapsing layout's minimum height
        // (its collapsed height) is the toolbar's, so the bar would then have
        // no range to collapse through and nowhere to draw the expanded title.
        val pinned = CollapsingToolbarLayout.LayoutParams(match, actionBarHeight(context))
        pinned.collapseMode = CollapsingToolbarLayout.LayoutParams.COLLAPSE_MODE_PIN
        collapsing.addView(toolbar, pinned)
        val scrolls = AppBarLayout.LayoutParams(match, mediumBarHeight(context))
        scrolls.scrollFlags = AppBarLayout.LayoutParams.SCROLL_FLAG_SCROLL or
            AppBarLayout.LayoutParams.SCROLL_FLAG_EXIT_UNTIL_COLLAPSED
        appBar.addView(collapsing, scrolls)
        val behavior = AppBarLayout.Behavior()
        // Only [applyCollapse] moves the bar: a drag on it would leave the
        // scroll signal and the bar disagreeing.
        behavior.setDragCallback(
            object : AppBarLayout.Behavior.DragCallback() {
                override fun canDrag(appBarLayout: AppBarLayout): Boolean = false
            },
        )
        val coordinator = CoordinatorLayout(context)
        coordinator.fitsSystemWindows = false
        val params = CoordinatorLayout.LayoutParams(match, wrap)
        params.behavior = behavior
        coordinator.addView(appBar, params)
        return TopBar(coordinator, appBar, toolbar, collapsing)
    }

    /** The medium collapsing bar's expanded height: the theme's `collapsingToolbarLayoutMediumSize`, else [MEDIUM_BAR_DP]. */
    private fun mediumBarHeight(context: Context): Int {
        val attrs = context.obtainStyledAttributes(intArrayOf(com.google.android.material.R.attr.collapsingToolbarLayoutMediumSize))
        return try {
            attrs.getDimensionPixelSize(0, dp(context, MEDIUM_BAR_DP))
        } finally {
            attrs.recycle()
        }
    }

    /** The collapsed bar's toolbar height: the theme's `actionBarSize`, else [SMALL_BAR_DP]. */
    private fun actionBarHeight(context: Context): Int {
        val attrs = context.obtainStyledAttributes(intArrayOf(androidx.appcompat.R.attr.actionBarSize))
        return try {
            attrs.getDimensionPixelSize(0, dp(context, SMALL_BAR_DP))
        } finally {
            attrs.recycle()
        }
    }

    private fun attach(installed: Installed) {
        val (content, root) = installed.content to installed.root
        attachBars(installed)
        ViewCompat.setOnApplyWindowInsetsListener(root) { _, insets -> rootInsets(installed, insets) }
        root.addOnAttachStateChangeListener(detachWatch)
        val observer = content.viewTreeObserver
        observer.addOnPreDrawListener(preDraw)
        installed.observer = observer
        if (root.id == View.NO_ID) root.id = View.generateViewId()
        ViewCompat.requestApplyInsets(content)
    }

    private fun attachBars(installed: Installed) {
        val bars = installed.bars
        val match = ViewGroup.LayoutParams.MATCH_PARENT
        val wrap = ViewGroup.LayoutParams.WRAP_CONTENT
        attachTopBar(installed, bars.top, 0)
        installed.content.addView(bars.bottom, 1, FrameLayout.LayoutParams(match, wrap, Gravity.BOTTOM))
        // Last, so it draws over the root; the insets listener moves it off the bars.
        val margin = dp(bars.context, FAB_MARGIN_DP)
        val fabParams = FrameLayout.LayoutParams(wrap, wrap, Gravity.BOTTOM or Gravity.END)
        fabParams.marginEnd = margin
        fabParams.bottomMargin = margin
        installed.content.addView(bars.fabs, fabParams)
        bars.bottom.addOnLayoutChangeListener(bottomHeightWatch)
        bars.bottom.setOnItemSelectedListener { item ->
            if (!applyingSelection) emitTab(item.itemId - 1, reselect = false)
            applyingSelection
        }
        bars.bottom.setOnItemReselectedListener { item ->
            if (!applyingSelection) emitTab(item.itemId - 1, reselect = true)
        }
        bars.bottom.accessibilityTraversalAfter = installed.root.id
    }

    /** Add `top` to the content at `index` (before the root) and wire its insets and menu taps. */
    private fun attachTopBar(installed: Installed, top: TopBar, index: Int) {
        val match = ViewGroup.LayoutParams.MATCH_PARENT
        val wrap = ViewGroup.LayoutParams.WRAP_CONTENT
        installed.content.addView(top.outer, index, FrameLayout.LayoutParams(match, wrap, Gravity.TOP))
        ViewCompat.setOnApplyWindowInsetsListener(top.appBar) { view, insets ->
            val sides = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
            view.setPadding(sides.left, sides.top, sides.right, 0)
            insets
        }
        top.toolbar.setOnMenuItemClickListener { item ->
            onActionClicked(item.itemId - 1)
            true
        }
    }

    private fun rebuildBars(installed: Installed) {
        val fresh = try {
            buildBars(installed.activity, installed.model.dark, installed.model.largeTitle)
        } catch (t: Throwable) {
            Log.w(TAG, "Frust: native app chrome kept its old brightness — rebuilding the bars failed", t)
            return
        }
        dismissPopup()
        removeBars(installed)
        installed.bars = fresh
        if (installed.lifted) fresh.appBar.setLifted(true)
        attachBars(installed)
        applyAll(installed)
        ViewCompat.requestApplyInsets(installed.content)
    }

    /**
     * Swap the top bar for the small or the collapsing one the model's
     * large-title flag names, leaving the bottom bar and the toolbar column
     * as they are; `false` when the shown bar already matches. The new bar is
     * built before anything is touched (a throw leaves the old one), takes
     * the old one's place, padding and lifted state in this same turn, and is
     * measured and laid out before the next draw, whose pre-draw pass pads
     * the root to it — so no frame shows either bar half-built. The old bar
     * leaves with its views; it held no listener on anything that stays.
     */
    private fun swapTopBar(installed: Installed): Boolean {
        val bars = installed.bars
        val old = bars.top
        val large = installed.model.largeTitle
        if ((old.collapsing != null) == large) return false
        val fresh = buildTopBar(bars.context, large)
        dismissPopup()
        val index = max(0, installed.content.indexOfChild(old.outer))
        releaseTopBar(old)
        installed.content.removeView(old.outer)
        bars.top = fresh
        // The insets pass below re-pads it; until then it keeps the old bar's status-bar padding.
        fresh.appBar.setPadding(old.appBar.paddingLeft, old.appBar.paddingTop, old.appBar.paddingRight, 0)
        if (installed.lifted) fresh.appBar.setLifted(true)
        attachTopBar(installed, fresh, index)
        applyTop(installed)
        applyCollapse(installed)
        ViewCompat.requestApplyInsets(installed.content)
        return true
    }

    /**
     * Unhook a top bar that is leaving: its insets listener, menu taps and
     * badge bookkeeping. Its badges ride its own menu items' overlays and go
     * with them; its navigation listener only reaches [emit] through [live].
     */
    private fun releaseTopBar(top: TopBar) {
        ViewCompat.setOnApplyWindowInsetsListener(top.appBar, null)
        top.toolbar.setOnMenuItemClickListener(null)
        top.actionBadges.clear()
    }

    private fun removeBars(installed: Installed) {
        val bars = installed.bars
        bars.bottom.removeOnLayoutChangeListener(bottomHeightWatch)
        installed.content.removeView(bars.top.outer)
        installed.content.removeView(bars.bottom)
        installed.content.removeView(bars.fabs)
    }

    private fun detachQuietly(installed: Installed) {
        try {
            dismissPopup()
            releaseRoot(installed)
            removeBars(installed)
            ViewCompat.requestApplyInsets(installed.content)
        } catch (t: Throwable) {
            Log.w(TAG, "Frust: native app chrome teardown incomplete", t)
        }
    }

    /** Undo everything done to the content root; never mutates the hierarchy. */
    private fun releaseRoot(installed: Installed) {
        installed.observer?.let { if (it.isAlive) it.removeOnPreDrawListener(preDraw) }
        installed.observer = null
        val root = installed.root
        root.removeOnAttachStateChangeListener(detachWatch)
        ViewCompat.setOnApplyWindowInsetsListener(root, null)
        val base = installed.basePadding
        root.setPadding(base[0], base[1], base[2], base[3])
        root.id = installed.baseRootId
    }

    /** The Activity or its content went away under an installed chrome: report it once. */
    private fun reportLost(installed: Installed) {
        if (live !== installed) return
        live = null
        try {
            dismissPopup()
            releaseRoot(installed)
        } catch (t: Throwable) {
            Log.w(TAG, "Frust: native app chrome release after host loss incomplete", t)
        }
        nativeOnChromeLost(installed.generation)
    }

    private val detachWatch =
        object : View.OnAttachStateChangeListener {
            override fun onViewAttachedToWindow(v: View) {}

            override fun onViewDetachedFromWindow(v: View) {
                val installed = live ?: return
                if (installed.root === v) reportLost(installed)
            }
        }

    private val preDraw = ViewTreeObserver.OnPreDrawListener { syncRootPadding() }

    private val bottomHeightWatch =
        View.OnLayoutChangeListener { _, _, top, _, bottom, _, oldTop, _, oldBottom ->
            val installed = live
            if (installed != null) {
                if (bottom - top != oldBottom - oldTop) ViewCompat.requestApplyInsets(installed.content)
                // A nav laid out after the last insets pass still moves the FAB column.
                repositionToolbar(installed)
            }
        }

    /** [positionToolbar] from the window's current insets, for callers outside an insets pass. */
    private fun repositionToolbar(installed: Installed) {
        val insets = ViewCompat.getRootWindowInsets(installed.content) ?: return
        positionToolbar(installed, insets)
    }

    /**
     * Pad the root to the bars' laid-out heights (a collapsing bar's visible
     * one, [shownHeight]); `false` cancels a draw that would show a stale size.
     */
    private fun syncRootPadding(): Boolean {
        val installed = live ?: return true
        val bars = installed.bars
        val base = installed.basePadding
        val top = base[1] + if (bars.top.outer.visibility == View.VISIBLE) shownHeight(bars.top) else 0
        val bottom = base[3] + if (bars.bottom.visibility == View.VISIBLE) bars.bottom.height else 0
        val root = installed.root
        if (root.paddingTop == top && root.paddingBottom == bottom) return true
        root.setPadding(base[0], top, base[2], bottom)
        return false
    }

    /**
     * How much of the content's top `top` covers: the small bar's height, or
     * the collapsing bar's bottom edge — its expanded height less how far the
     * Behavior has offset it up (laid out at the coordinator's top).
     */
    private fun shownHeight(top: TopBar): Int =
        if (top.collapsing == null) top.appBar.height else max(0, top.appBar.bottom)

    private fun rootInsets(installed: Installed, insets: WindowInsetsCompat): WindowInsetsCompat {
        val bars = installed.bars
        // Keyboard up -> bottom navigation hidden; applied on a transition only, no relayout churn.
        val imeVisible = insets.isVisible(WindowInsetsCompat.Type.ime())
        if (imeVisible != installed.imeVisible) {
            installed.imeVisible = imeVisible
            applyBottomVisibility(installed)
        }
        val topCovered = bars.top.outer.visibility == View.VISIBLE
        val bottomCovered = bars.bottom.visibility == View.VISIBLE
        val builder = WindowInsetsCompat.Builder(insets)
        val sideTypes = intArrayOf(
            WindowInsetsCompat.Type.statusBars(),
            WindowInsetsCompat.Type.navigationBars(),
            WindowInsetsCompat.Type.captionBar(),
            WindowInsetsCompat.Type.displayCutout(),
        )
        for (type in sideTypes) {
            val i = insets.getInsets(type)
            builder.setInsets(type, Insets.of(i.left, if (topCovered) 0 else i.top, i.right, if (bottomCovered) 0 else i.bottom))
        }
        val cutout = insets.displayCutout
        if (cutout != null && Build.VERSION.SDK_INT >= 28) {
            val safe = Rect(
                cutout.safeInsetLeft,
                if (topCovered) 0 else cutout.safeInsetTop,
                cutout.safeInsetRight,
                if (bottomCovered) 0 else cutout.safeInsetBottom,
            )
            builder.setDisplayCutout(DisplayCutoutCompat(safe, cutout.boundingRects))
        }
        val ime = insets.getInsets(WindowInsetsCompat.Type.ime())
        val imeBottom = if (bottomCovered) max(0, ime.bottom - bars.bottom.height) else ime.bottom
        builder.setInsets(WindowInsetsCompat.Type.ime(), Insets.of(ime.left, ime.top, ime.right, imeBottom))
        positionToolbar(installed, insets)
        return builder.build()
    }

    /**
     * Keep the toolbar column [FAB_MARGIN_DP] above the bottom navigation while
     * it shows, else above the larger of the IME and navigation-bar insets,
     * and that far in from the trailing edge's system insets.
     */
    private fun positionToolbar(installed: Installed, insets: WindowInsetsCompat) {
        val bars = installed.bars
        val margin = dp(bars.context, FAB_MARGIN_DP)
        val below = if (bars.bottom.visibility == View.VISIBLE) {
            bars.bottom.height
        } else {
            val ime = insets.getInsets(WindowInsetsCompat.Type.ime()).bottom
            max(ime, insets.getInsets(WindowInsetsCompat.Type.navigationBars()).bottom)
        }
        val sides = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
        val rtl = installed.content.layoutDirection == View.LAYOUT_DIRECTION_RTL
        val end = margin + if (rtl) sides.left else sides.right
        val params = bars.fabs.layoutParams as? FrameLayout.LayoutParams ?: return
        if (params.bottomMargin == below + margin && params.marginEnd == end) return
        params.bottomMargin = below + margin
        params.marginEnd = end
        bars.fabs.layoutParams = params
    }

    // --- applying the spec ---------------------------------------------------

    /** Update the views for the `changed` fields of the already merged model. */
    private fun applyChanged(installed: Installed, changed: Set<String>) {
        if (FIELD_BRIGHTNESS in changed) {
            rebuildBars(installed)
            return
        }
        // A swapped top bar was filled from the whole merged model already.
        val swapped = FIELD_LARGE_TITLE in changed && swapTopBar(installed)
        if (!swapped) {
            if (FIELD_TOP_BAR in changed) applyTopVisibility(installed)
            if (FIELD_TITLE in changed || FIELD_SUBTITLE in changed) applyTitles(installed)
            if (FIELD_LEADING in changed) applyLeading(installed)
        }
        if (FIELD_ACTIONS in changed) {
            if (!swapped) applyActions(installed)
            applyToolbar(installed)
        } else if (FIELD_ACTION_BADGES in changed) {
            // In place: the menu items and buttons stay, only their badges move.
            if (!swapped) applyActionBadges(installed)
            applyToolbarBadges(installed)
        }
        if (FIELD_TABS in changed) applyTabs(installed) else if (FIELD_SELECTED in changed) applySelected(installed)
    }

    private fun applyAll(installed: Installed) {
        applyTop(installed)
        applyToolbar(installed)
        applyTabs(installed)
        applyCollapse(installed)
    }

    /** Everything the top bar shows, from the merged model. */
    private fun applyTop(installed: Installed) {
        applyTopVisibility(installed)
        applyTitles(installed)
        applyLeading(installed)
        applyActions(installed)
    }

    private fun applyTopVisibility(installed: Installed) {
        val visibility = if (installed.model.hasTop) View.VISIBLE else View.GONE
        val outer = installed.bars.top.outer
        if (outer.visibility != visibility) {
            outer.visibility = visibility
            ViewCompat.requestApplyInsets(installed.content)
        }
    }

    private fun applyTitles(installed: Installed) {
        val top = installed.bars.top
        val collapsing = top.collapsing
        if (collapsing == null) {
            top.toolbar.title = installed.model.title
            top.toolbar.subtitle = installed.model.subtitle
        } else {
            // The collapsing layout draws both, expanded and collapsed; a
            // toolbar title would draw a second copy over the collapsed one.
            top.toolbar.title = null
            top.toolbar.subtitle = null
            collapsing.title = installed.model.title
            collapsing.subtitle = installed.model.subtitle
        }
    }

    /**
     * Collapse a large title while the body is lifted, expand it at the top;
     * animated once the bar is laid out, else jumped to (a fresh install or
     * swap over a scrolled body). An expanded bar collapses only when the
     * body's `max` exceeds the collapse range: the body grows by that range
     * as the bar collapses, and a body that would then have nothing to scroll
     * would snap back to the top and expand the bar again.
     */
    private fun applyCollapse(installed: Installed) {
        val top = installed.bars.top
        val collapsing = top.collapsing ?: return
        val range = top.appBar.totalScrollRange.takeIf { it > 0 }
            ?: max(0, collapsing.layoutParams.height - top.toolbar.layoutParams.height)
        val rangeLogical = range / installed.bars.context.resources.displayMetrics.density
        val collapse = installed.lifted && (top.collapsed || installed.scrollMax > rangeLogical)
        if (collapse == top.collapsed) return
        top.collapsed = collapse
        top.appBar.setExpanded(!collapse, top.appBar.isLaidOut)
    }

    private fun applyLeading(installed: Installed) {
        val toolbar = installed.bars.toolbar
        val context = installed.bars.context
        when (installed.model.leading) {
            LEADING_BACK -> {
                toolbar.setNavigationIconTint(MaterialColors.getColor(toolbar, com.google.android.material.R.attr.colorOnSurface))
                toolbar.setNavigationIcon(androidx.appcompat.R.drawable.abc_ic_ab_back_material)
                toolbar.setNavigationContentDescription(androidx.appcompat.R.string.abc_action_bar_up_description)
                toolbar.setNavigationOnClickListener { emit(FrustNativeListener.KIND_CHROME_BACK, 0L) }
            }
            LEADING_IMAGE -> {
                val bitmap = icons[iconKey(ICON_SLOT_LEADING, 0)]
                toolbar.setNavigationOnClickListener(null)
                toolbar.clearNavigationIconTint()
                toolbar.navigationIcon = bitmap?.let {
                    val size = dp(context, AVATAR_DP)
                    RoundedBitmapDrawableFactory.create(context.resources, squareScaled(it, size)).apply { isCircular = true }
                }
            }
            LEADING_ICON -> {
                // A plain icon, tinted like an action and not tappable (the iOS leading symbol).
                val bitmap = icons[iconKey(ICON_SLOT_LEADING, 0)]
                toolbar.setNavigationOnClickListener(null)
                toolbar.setNavigationContentDescription(null)
                toolbar.navigationIcon = bitmap?.let { BitmapDrawable(context.resources, fitScaled(it, dp(context, ACTION_ICON_DP))) }
                toolbar.setNavigationIconTint(MaterialColors.getColor(toolbar, com.google.android.material.R.attr.colorOnSurface))
            }
            else -> {
                toolbar.setNavigationOnClickListener(null)
                toolbar.navigationIcon = null
            }
        }
    }

    private fun applyActions(installed: Installed) {
        dismissPopup()
        val bars = installed.bars
        val menu = bars.toolbar.menu
        detachActionBadges(bars)
        menu.clear()
        val tint = ColorStateList.valueOf(MaterialColors.getColor(bars.toolbar, com.google.android.material.R.attr.colorOnSurfaceVariant))
        val primary = MaterialColors.getColor(bars.toolbar, androidx.appcompat.R.attr.colorPrimary)
        installed.model.actions.forEachIndexed { index, action ->
            // A prominent title-only item is drawn in the primary colour.
            val title: CharSequence = if (action.prominent) tinted(action.title, primary) else action.title
            val item = menu.add(Menu.NONE, index + 1, index, title)
            item.setShowAsAction(MenuItem.SHOW_AS_ACTION_ALWAYS)
            item.contentDescription = action.title
            val bitmap = icons[iconKey(ICON_SLOT_ACTION, index)]
            if (action.hasIcon && bitmap != null) {
                item.icon = BitmapDrawable(bars.context.resources, fitScaled(bitmap, dp(bars.context, ACTION_ICON_DP)))
                item.iconTintList = if (action.prominent) ColorStateList.valueOf(primary) else tint
            }
        }
        applyActionBadges(installed)
    }

    /**
     * Bring the menu items' badges to the model in place: update a badge that
     * stays, attach a new one, detach a gone one. Attaching through the
     * toolbar waits for the item's view (Material posts it), so a detach is
     * posted too: it then runs after any attach still pending.
     */
    @androidx.annotation.OptIn(markerClass = [ExperimentalBadgeUtils::class])
    private fun applyActionBadges(installed: Installed) {
        val bars = installed.bars
        val actions = installed.model.actions
        for (id in bars.actionBadges.keys.toList()) {
            if (actions.getOrNull(id - 1)?.badge == null) {
                val drawable = bars.actionBadges.remove(id) ?: continue
                postDetach(bars, drawable, id)
            }
        }
        actions.forEachIndexed { index, action ->
            val badge = action.badge ?: return@forEachIndexed
            val id = index + 1
            val held = bars.actionBadges[id]
            if (held != null) {
                fillBadge(held, badge)
            } else {
                val drawable = BadgeDrawable.create(bars.context)
                fillBadge(drawable, badge)
                bars.actionBadges[id] = drawable
                BadgeUtils.attachBadgeDrawable(drawable, bars.toolbar, id)
            }
        }
    }

    /** Detach every menu-item badge, before the menu is rebuilt. */
    private fun detachActionBadges(bars: Bars) {
        for ((id, drawable) in bars.actionBadges) postDetach(bars, drawable, id)
        bars.actionBadges.clear()
    }

    @androidx.annotation.OptIn(markerClass = [ExperimentalBadgeUtils::class])
    private fun postDetach(bars: Bars, drawable: BadgeDrawable, id: Int) {
        val toolbar = bars.toolbar
        toolbar.post {
            try {
                BadgeUtils.detachBadgeDrawable(drawable, toolbar, id)
            } catch (t: Throwable) {
                Log.w(TAG, "Frust: native app chrome badge detach failed", t)
            }
        }
    }

    /** Rebuild the toolbar column from the model: items stacked above the primary; GONE when empty. */
    private fun applyToolbar(installed: Installed) {
        dismissPopup()
        val bars = installed.bars
        val context = bars.context
        val stack = bars.fabs
        detachToolbarBadges(bars)
        stack.removeAllViews()
        val entries = installed.model.toolbar
        stack.visibility = if (entries.isEmpty()) View.GONE else View.VISIBLE
        val wrap = ViewGroup.LayoutParams.WRAP_CONTENT
        val fill = ColorStateList.valueOf(MaterialColors.getColor(stack, androidx.appcompat.R.attr.colorPrimary))
        val onFill = ColorStateList.valueOf(MaterialColors.getColor(stack, com.google.android.material.R.attr.colorOnPrimary))
        entries.forEachIndexed { t, entry ->
            val primary = installed.model.toolbarPrimary && t == entries.lastIndex
            val bitmap = icons[iconKey(ICON_SLOT_TOOLBAR, t)]
            val icon = if (entry.hasIcon && bitmap != null) {
                BitmapDrawable(context.resources, fitScaled(bitmap, dp(context, ACTION_ICON_DP)))
            } else {
                null
            }
            val button: View = if (primary && icon != null) {
                FloatingActionButton(context).apply {
                    setImageDrawable(icon)
                    if (entry.prominent) {
                        backgroundTintList = fill
                        imageTintList = onFill
                    }
                }
            } else {
                ExtendedFloatingActionButton(context).apply {
                    text = entry.title
                    this.icon = icon
                    if (entry.prominent) {
                        backgroundTintList = fill
                        setTextColor(onFill)
                        iconTint = onFill
                    }
                }
            }
            button.contentDescription = entry.title
            button.setOnClickListener { onToolbarClicked(t, it) }
            val params = LinearLayout.LayoutParams(wrap, wrap)
            params.gravity = Gravity.END
            if (t > 0) params.topMargin = dp(context, FAB_GAP_DP)
            stack.addView(button, params)
        }
        applyToolbarBadges(installed)
    }

    /**
     * Bring the toolbar buttons' badges to the model in place, like
     * [applyActionBadges]. A button's badge rides its overlay and follows its
     * layout ([fabBadgeBounds]).
     */
    @androidx.annotation.OptIn(markerClass = [ExperimentalBadgeUtils::class])
    private fun applyToolbarBadges(installed: Installed) {
        val bars = installed.bars
        val entries = installed.model.toolbar
        for (t in bars.fabBadges.keys.toList()) {
            if (entries.getOrNull(t)?.badge == null) {
                val drawable = bars.fabBadges.remove(t) ?: continue
                bars.fabs.getChildAt(t)?.let { BadgeUtils.detachBadgeDrawable(drawable, it) }
            }
        }
        entries.forEachIndexed { t, entry ->
            val badge = entry.badge ?: return@forEachIndexed
            val held = bars.fabBadges[t]
            if (held != null) {
                fillBadge(held, badge)
                return@forEachIndexed
            }
            val button = bars.fabs.getChildAt(t) ?: return@forEachIndexed
            val drawable = BadgeDrawable.create(bars.context)
            fillBadge(drawable, badge)
            bars.fabBadges[t] = drawable
            BadgeUtils.attachBadgeDrawable(drawable, button)
            button.addOnLayoutChangeListener(fabBadgeBounds)
        }
    }

    /** Detach every toolbar-button badge, before the buttons are rebuilt. */
    @androidx.annotation.OptIn(markerClass = [ExperimentalBadgeUtils::class])
    private fun detachToolbarBadges(bars: Bars) {
        for ((t, drawable) in bars.fabBadges) {
            bars.fabs.getChildAt(t)?.let { BadgeUtils.detachBadgeDrawable(drawable, it) }
        }
        bars.fabBadges.clear()
    }

    /** Re-anchors a toolbar button's badge once the button is laid out. */
    private val fabBadgeBounds =
        View.OnLayoutChangeListener { view, _, _, _, _, _, _, _, _ ->
            val bars = live?.bars ?: return@OnLayoutChangeListener
            val t = bars.fabs.indexOfChild(view)
            bars.fabBadges[t]?.let { BadgeUtils.setBadgeDrawableBounds(it, view, null) }
        }

    /** `text` drawn in `color`. */
    private fun tinted(text: String, color: Int): CharSequence =
        SpannableString(text).apply {
            setSpan(ForegroundColorSpan(color), 0, length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
        }

    private fun onActionClicked(index: Int) {
        val installed = live ?: return
        val action = installed.model.actions.getOrNull(index) ?: return
        val bars = installed.bars
        val anchor = bars.toolbar.findViewById<View?>(index + 1) ?: bars.toolbar
        reportOrShowMenu(installed, index, action, anchor)
    }

    /** Toolbar entry `t`, whose report index follows the top bar's actions. */
    private fun onToolbarClicked(t: Int, anchor: View) {
        val installed = live ?: return
        val entry = installed.model.toolbar.getOrNull(t) ?: return
        reportOrShowMenu(installed, installed.model.actions.size + t, entry, anchor)
    }

    /** Report action `index`, or pop its menu up from `anchor`, each choice reporting its item. */
    private fun reportOrShowMenu(installed: Installed, index: Int, action: Action, anchor: View) {
        if (action.menu.isEmpty()) {
            emit(FrustNativeListener.KIND_CHROME_ACTION, index.toLong())
            return
        }
        val bars = installed.bars
        val shown = PopupMenu(bars.context, anchor, Gravity.END)
        val danger = MaterialColors.getColor(bars.toolbar, androidx.appcompat.R.attr.colorError)
        action.menu.forEachIndexed { m, entry ->
            val title: CharSequence = if (entry.destructive) tinted(entry.title, danger) else entry.title
            shown.menu.add(Menu.NONE, m + 1, m, title).isEnabled = entry.enabled
        }
        shown.setOnMenuItemClickListener { item ->
            emit(FrustNativeListener.KIND_CHROME_MENU, index.toLong() * CHROME_MENU_STRIDE + (item.itemId - 1))
            true
        }
        shown.setOnDismissListener { if (popup === it) popup = null }
        popup = shown
        shown.show()
    }

    private fun applyTabs(installed: Installed) {
        val bottom = installed.bars.bottom
        val tabs = installed.model.tabs.take(bottom.maxItemCount)
        if (tabs.size < installed.model.tabs.size) {
            Log.w(TAG, "Frust: native app chrome shows the first ${bottom.maxItemCount} of ${installed.model.tabs.size} tabs")
        }
        applyingSelection = true
        try {
            bottom.menu.clear()
            tabs.forEachIndexed { index, tab ->
                bottom.menu.add(Menu.NONE, index + 1, index, tab.title).isEnabled = tab.enabled
                refreshTabIcon(installed, index)
                applyBadge(bottom, index + 1, tab.badge)
            }
        } finally {
            applyingSelection = false
        }
        applyBottomVisibility(installed)
        applySelected(installed)
    }

    /** GONE without tabs or while the keyboard is up, else VISIBLE; only a change touches the view. */
    private fun applyBottomVisibility(installed: Installed) {
        val bottom = installed.bars.bottom
        val hidden = installed.model.tabs.isEmpty() || installed.imeVisible
        val visibility = if (hidden) View.GONE else View.VISIBLE
        if (bottom.visibility != visibility) {
            bottom.visibility = visibility
            ViewCompat.requestApplyInsets(installed.content)
            repositionToolbar(installed)
        }
    }

    private fun refreshTabIcon(installed: Installed, index: Int) {
        val tab = installed.model.tabs.getOrNull(index) ?: return
        val item = installed.bars.bottom.menu.findItem(index + 1) ?: return
        val bitmap = icons[iconKey(ICON_SLOT_TAB, index)]
        item.icon = if (tab.iconRev != 0L && bitmap != null) BitmapDrawable(installed.bars.context.resources, bitmap) else null
    }

    private fun applyBadge(bottom: BottomNavigationView, itemId: Int, badge: String?) {
        if (badge == null) {
            bottom.removeBadge(itemId)
            return
        }
        fillBadge(bottom.getOrCreateBadge(itemId), badge)
    }

    /** Show `badge` on `drawable`: `""` is a dot, a non-negative number a count, anything else its text. */
    private fun fillBadge(drawable: BadgeDrawable, badge: String) {
        val number = badge.toIntOrNull()
        drawable.clearNumber()
        drawable.clearText()
        when {
            badge.isEmpty() -> Unit
            number != null && number >= 0 -> drawable.number = number
            else -> drawable.text = badge
        }
        drawable.isVisible = true
    }

    /** Show the app-confirmed tab; the selection listener accepts it under [applyingSelection]. */
    private fun applySelected(installed: Installed) {
        val bottom = installed.bars.bottom
        val menu = bottom.menu
        val index = installed.model.selected
        applyingSelection = true
        try {
            if (index in 0 until menu.size()) {
                bottom.selectedItemId = index + 1
            } else {
                menu.setGroupCheckable(Menu.NONE, true, false)
                for (i in 0 until menu.size()) menu.getItem(i).isChecked = false
                menu.setGroupCheckable(Menu.NONE, true, true)
            }
        } finally {
            applyingSelection = false
        }
    }

    // --- reporting -----------------------------------------------------------

    private fun emitTab(index: Int, reselect: Boolean) {
        if (index < 0) return
        val detail = index.toLong() or (if (reselect) 1L shl CHROME_TAB_RESELECT_SHIFT else 0L)
        emit(FrustNativeListener.KIND_CHROME_TAB, detail)
    }

    private fun emit(kind: Int, detail: Long) {
        val installed = live ?: return
        nativeOnChromeEvent(installed.generation, kind, detail)
    }

    // --- helpers -------------------------------------------------------------

    private fun warnTheme(t: Throwable) {
        Log.w(
            TAG,
            "Frust: native app chrome refused — the app theme must derive from Theme.Material3.* " +
                "(or Theme.MaterialComponents.*): set android:theme on <application> and <activity>, " +
                "e.g. @style/Theme.Material3.DayNight.NoActionBar. Falling back to frust-drawn bars. ($t)",
        )
    }

    private fun dismissPopup() {
        val shown = popup ?: return
        popup = null
        try {
            shown.dismiss()
        } catch (t: Throwable) {
            // Its window may already be gone.
        }
    }

    private fun assertMainThread() {
        if (debuggable) check(Looper.myLooper() == Looper.getMainLooper()) { "FrustNativeChrome called off the main thread" }
    }

    /** [setIcon]'s decode cap for `slot`: 4x its on-screen size, at least [MIN_ICON_DECODE_PX]. */
    private fun iconDecodeCap(slot: Int): Int {
        val dp = when (slot) {
            ICON_SLOT_LEADING -> AVATAR_DP
            ICON_SLOT_TAB -> TAB_ICON_DP
            else -> ACTION_ICON_DP
        }
        val px = (dp * Resources.getSystem().displayMetrics.density).roundToInt()
        return max(4 * px, MIN_ICON_DECODE_PX)
    }

    /**
     * `bytes` decoded with a power-of-two `inSampleSize` that brings the
     * longer side to at most `cap` px (bounds read first, so an oversized
     * image never allocates full size); `null` when the bytes do not decode.
     */
    private fun decodeIcon(bytes: ByteArray, cap: Int): Bitmap? = try {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
        if (bounds.outWidth <= 0 || bounds.outHeight <= 0) {
            null
        } else {
            val longer = max(bounds.outWidth, bounds.outHeight)
            var sample = 1
            while ((longer + sample - 1) / sample > cap) sample *= 2
            BitmapFactory.decodeByteArray(bytes, 0, bytes.size, BitmapFactory.Options().apply { inSampleSize = sample })
        }
    } catch (t: Throwable) {
        Log.w(TAG, "Frust: native app chrome icon decode threw", t)
        null
    }

    private fun iconKey(slot: Int, index: Int): Long = (slot.toLong() shl 32) or (index.toLong() and 0xFFFFFFFFL)

    private fun dp(context: Context, value: Int): Int = (value * context.resources.displayMetrics.density).roundToInt()

    /** `bitmap` scaled to fit a `size`-px square, aspect kept. */
    private fun fitScaled(bitmap: Bitmap, size: Int): Bitmap {
        val scale = size.toFloat() / max(bitmap.width, bitmap.height)
        if (scale == 1f) return bitmap
        val w = max(1, (bitmap.width * scale).roundToInt())
        val h = max(1, (bitmap.height * scale).roundToInt())
        return Bitmap.createScaledBitmap(bitmap, w, h, true)
    }

    /** `bitmap` centre-cropped to a square and scaled to `size` px. */
    private fun squareScaled(bitmap: Bitmap, size: Int): Bitmap {
        val side = minOf(bitmap.width, bitmap.height)
        val square = Bitmap.createBitmap(bitmap, (bitmap.width - side) / 2, (bitmap.height - side) / 2, side, side)
        return Bitmap.createScaledBitmap(square, size, size, true)
    }
}
