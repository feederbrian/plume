// Plume Floats — GNOME Shell extension (GNOME 45+).
//
// Wayland deliberately gives applications no way to keep their own window on
// top or choose where it appears. The compositor can, so this extension does
// it for Plume's floating readouts only: it matches windows by application id
// and title prefix (see TITLE_PREFIX in crates/plume/src/floats.rs), keeps
// them above and on all workspaces, and restores their last position.

import GLib from 'gi://GLib';
import Gio from 'gi://Gio';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const APP_ID = 'io.github.feederbrian.Plume';
const TITLE_PREFIX = 'Plume Float · ';

export default class PlumeFloatsExtension extends Extension {
    enable() {
        this._tracked = new Map();    // MetaWindow -> [handler ids]
        this._applied = new WeakSet();
        this._positions = this._load();
        this._saveId = 0;
        this._createdId = global.display.connect('window-created', (_display, win) => this._track(win));
        for (const actor of global.get_window_actors())
            this._track(actor.meta_window);
    }

    disable() {
        if (this._createdId) {
            global.display.disconnect(this._createdId);
            this._createdId = 0;
        }
        for (const [win, ids] of this._tracked) {
            for (const id of ids)
                win.disconnect(id);
            if (this._applied.has(win)) {
                if (win.is_above())
                    win.unmake_above();
                win.unstick();
            }
        }
        this._tracked.clear();
        if (this._saveId) {
            GLib.source_remove(this._saveId);
            this._saveId = 0;
            this._save();
        }
        this._positions = null;
    }

    _isFloat(win) {
        if (!win)
            return false;
        const app = win.get_gtk_application_id?.() ?? win.get_wm_class?.() ?? '';
        const title = win.get_title() ?? '';
        return (app === APP_ID || win.get_wm_class?.() === APP_ID) && title.startsWith(TITLE_PREFIX);
    }

    _key(win) {
        return (win.get_title() ?? '').slice(TITLE_PREFIX.length);
    }

    _track(win) {
        if (!win || this._tracked.has(win))
            return;
        const ids = [];
        ids.push(win.connect('notify::title', () => this._apply(win)));
        ids.push(win.connect('position-changed', () => {
            if (this._applied.has(win))
                this._remember(win);
        }));
        ids.push(win.connect('unmanaged', () => {
            for (const id of this._tracked.get(win) ?? [])
                win.disconnect(id);
            this._tracked.delete(win);
        }));
        this._tracked.set(win, ids);
        this._apply(win);
    }

    _apply(win) {
        // The title may arrive after the window is created, hence notify::title.
        if (this._applied.has(win) || !this._isFloat(win))
            return;
        this._applied.add(win);
        win.make_above();
        win.stick();
        const pos = this._positions[this._key(win)];
        if (!pos)
            return;
        const move = () => {
            if (this._tracked.has(win))
                win.move_frame(true, pos.x, pos.y);
        };
        // Move once Mutter has placed and drawn the window.
        const actor = win.get_compositor_private();
        if (actor) {
            const id = actor.connect('first-frame', () => {
                actor.disconnect(id);
                move();
            });
        }
        GLib.idle_add(GLib.PRIORITY_DEFAULT_IDLE, () => {
            move();
            return GLib.SOURCE_REMOVE;
        });
    }

    _remember(win) {
        const r = win.get_frame_rect();
        this._positions[this._key(win)] = {x: r.x, y: r.y};
        if (!this._saveId) {
            this._saveId = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, 1, () => {
                this._saveId = 0;
                this._save();
                return GLib.SOURCE_REMOVE;
            });
        }
    }

    _file() {
        return Gio.File.new_for_path(GLib.build_filenamev([GLib.get_user_config_dir(), 'plume', 'float-positions.json']));
    }

    _load() {
        try {
            const [ok, bytes] = this._file().load_contents(null);
            if (ok)
                return JSON.parse(new TextDecoder().decode(bytes)) ?? {};
        } catch (_e) {
            // No file yet, or unreadable: start fresh.
        }
        return {};
    }

    _save() {
        if (!this._positions)
            return;
        try {
            const f = this._file();
            const dir = f.get_parent();
            if (!dir.query_exists(null))
                dir.make_directory_with_parents(null);
            const data = new TextEncoder().encode(JSON.stringify(this._positions, null, 1));
            f.replace_contents(data, null, false, Gio.FileCreateFlags.REPLACE_DESTINATION, null);
        } catch (e) {
            console.warn(`Plume Floats: could not save positions: ${e.message}`);
        }
    }
}
