//! A layout that never reflows because of content: one or two equal-width
//! columns, chosen from the available width alone. (GtkFlowBox decides from
//! children's natural widths, so a label changing from "9.8 %" to "10 %"
//! could resize or move every card and the graphs inside them.)

use gtk::{glib, gsk, prelude::*, subclass::prelude::*};
use std::cell::{Cell, RefCell};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Tiles {
        pub children: RefCell<Vec<gtk::Widget>>,
        pub min_col: Cell<i32>,
        pub spacing: Cell<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Tiles {
        const NAME: &'static str = "PlumeTiles";
        type Type = super::Tiles;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Tiles {
        fn dispose(&self) {
            for c in self.children.take() {
                c.unparent();
            }
        }
    }

    impl Tiles {
        fn cols_for(&self, width: i32) -> i32 {
            let (m, s) = (self.min_col.get(), self.spacing.get());
            if width >= 2 * m + s { 2 } else { 1 }
        }

        /// Row heights for a given width (natural height of the tallest tile in each row).
        fn rows(&self, width: i32) -> (i32, i32, Vec<i32>) {
            let cols = self.cols_for(width);
            let s = self.spacing.get();
            let col_w = ((width - s * (cols - 1)) / cols).max(1);
            let kids = self.children.borrow();
            let rows = kids
                .chunks(cols as usize)
                .map(|row| {
                    row.iter()
                        .filter(|c| c.is_visible())
                        .map(|c| c.measure(gtk::Orientation::Vertical, col_w).1)
                        .max()
                        .unwrap_or(0)
                })
                .collect();
            (cols, col_w, rows)
        }
    }

    impl WidgetImpl for Tiles {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let (m, s) = (self.min_col.get(), self.spacing.get());
            match orientation {
                // Independent of content: one column minimum, two preferred.
                gtk::Orientation::Horizontal => (m, 2 * m + s, -1, -1),
                _ => {
                    let width = if for_size > 0 { for_size } else { 2 * m + s };
                    let (_, _, rows) = self.rows(width);
                    let h = rows.iter().sum::<i32>() + s * (rows.len() as i32 - 1).max(0);
                    (h, h, -1, -1)
                }
            }
        }

        fn size_allocate(&self, width: i32, _height: i32, _baseline: i32) {
            let (cols, col_w, rows) = self.rows(width);
            let s = self.spacing.get();
            let kids = self.children.borrow();
            let mut y = 0;
            for (r, row) in kids.chunks(cols as usize).enumerate() {
                let h = rows[r];
                for (c, child) in row.iter().enumerate() {
                    let x = c as i32 * (col_w + s);
                    let t = gsk::Transform::new().translate(&gtk::graphene::Point::new(x as f32, y as f32));
                    child.allocate(col_w, h, -1, Some(t));
                }
                y += h + s;
            }
        }
    }
}

glib::wrapper! {
    pub struct Tiles(ObjectSubclass<imp::Tiles>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Tiles {
    pub fn new(min_col: i32, spacing: i32) -> Tiles {
        let t: Tiles = glib::Object::new();
        t.imp().min_col.set(min_col);
        t.imp().spacing.set(spacing);
        t
    }

    pub fn append(&self, child: &impl IsA<gtk::Widget>) {
        child.set_parent(self);
        self.imp().children.borrow_mut().push(child.clone().upcast());
        self.queue_resize();
    }
}
