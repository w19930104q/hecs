use core::mem;

use alloc::vec::Vec;

use crate::{Component, Entity, PreparedQuery, With, Without, World};

/// Helper to track changes in `T` components
///
/// For each entity with a `T` component, a private component is inserted which stores the value as
/// of the most recent call to `track`. This provides robust, exact change detection at the cost of
/// visiting each possibly-changed entity. It is a good fit for entities that will typically be
/// visited regardless, and components having fast [`Clone`] and [`PartialEq`] impls. For components
/// which are expensive to compare and/or clone, consider instead tracking changes manually, e.g.
/// by setting a flag in the component's `DerefMut` implementation.
///
/// Always use exactly one `ChangeTracker` per [`World`] per component type of interest. Using
/// multiple trackers of the same `T` on the same world, or using the same tracker across multiple
/// worlds, will produce unpredictable results.
pub struct ChangeTracker<T: Component> {
    added: PreparedQuery<Without<(Entity, &'static T), &'static Previous<T>>>,
    changed: PreparedQuery<(Entity, &'static T, &'static mut Previous<T>)>,
    removed: PreparedQuery<Without<With<Entity, &'static Previous<T>>, &'static T>>,

    added_components: Vec<(Entity, T)>,
    removed_components: Vec<Entity>,
}

impl<T: Component> ChangeTracker<T> {
    /// Create a change tracker for `T` components
    pub fn new() -> Self {
        Self {
            added: PreparedQuery::new(),
            changed: PreparedQuery::new(),
            removed: PreparedQuery::new(),

            added_components: Vec::new(),
            removed_components: Vec::new(),
        }
    }

    /// Determine the changes in `T` components in `world` since the previous call
    pub fn track<'a>(&'a mut self, world: &'a mut World) -> Changes<'a, T>
    where
        T: Clone + PartialEq,
    {
        Changes {
            tracker: self,
            world,
            added: false,
            changed: false,
            removed: false,
        }
    }

    #[test]
    fn removed_after_component_removal() {
        let mut world = World::new();

        let a = world.spawn((42, true));
        let b = world.spawn((17, false));

        let mut tracker = ChangeTracker::<i32>::new();

        // First track: establish baseline
        {
            let mut changes = tracker.track(&mut world);
            assert_eq!(changes.added().count(), 2);
        }

        // Remove T from entity a
        world.remove_one::<i32>(a).unwrap();

        // Second track: should report a as removed
        {
            let mut changes = tracker.track(&mut world);
            let removed: Vec<_> = changes.removed().collect();
            assert_eq!(removed.len(), 1, "entity a should be reported as removed");
            assert_eq!(removed[0].0, a);
            assert_eq!(removed[0].1, 42);
            assert_eq!(changes.changed().count(), 0);
            assert_eq!(changes.added().count(), 0);
        }
    }

    #[test]
    fn added_after_insert_on_existing_entity() {
        let mut world = World::new();

        let a = world.spawn((true,));

        let mut tracker = ChangeTracker::<i32>::new();

        // First track: no i32 components exist yet
        {
            let mut changes = tracker.track(&mut world);
            assert_eq!(changes.added().count(), 0);
        }

        // Insert i32 into entity a (moves it from {bool} to {bool, i32})
        world.insert_one(a, 42).unwrap();

        // Second track: should report a as added
        {
            let mut changes = tracker.track(&mut world);
            let added: Vec<_> = changes.added().collect();
            assert_eq!(added.len(), 1, "entity a should be reported as added");
            assert_eq!(added[0].0, a);
            assert_eq!(*added[0].1, 42);
        }
    }

    #[test]
    fn change_detection_across_archetype_migration() {
        // Entity has T and is tracked. Then a different component is added,
        // causing the entity to move archetypes. T should still be tracked correctly.
        let mut world = World::new();

        let a = world.spawn((42,));

        let mut tracker = ChangeTracker::<i32>::new();

        // First track: establish baseline
        {
            let mut changes = tracker.track(&mut world);
            assert_eq!(changes.added().count(), 1);
        }

        // Modify T
        *world.get::<&mut i32>(a).unwrap() = 99;

        // Add a different component, causing archetype migration
        world.insert_one(a, true).unwrap();

        // Second track: should detect the change in T
        {
            let mut changes = tracker.track(&mut world);
            let changed: Vec<_> = changes.changed().collect();
            assert_eq!(changed.len(), 1, "entity a should be reported as changed");
            assert_eq!(changed[0].0, a);
            assert_eq!(changed[0].1, 42); // old value
            assert_eq!(*changed[0].2, 99); // new value
        }
    }

    #[test]
    fn same_slot_respawn() {
        // When an entity is despawned and a new one spawns in the same slot,
        // the change tracker should not carry over state from the old entity.
        let mut world = World::new();

        let a = world.spawn((42,));
        let a_id = a.id();
        let a_gen = a.generation;

        let mut tracker = ChangeTracker::<i32>::new();

        // Establish baseline
        {
            let mut changes = tracker.track(&mut world);
            assert_eq!(changes.added().count(), 1);
        }

        // Despawn and respawn in same slot
        world.despawn(a).unwrap();
        let b = world.spawn((100,));
        assert_eq!(b.id(), a_id);
        assert_ne!(b.generation, a_gen);

        // Track: should see b as added with 100, NOT as changed from 42
        {
            let mut changes = tracker.track(&mut world);
            let added: Vec<_> = changes.added().collect();
            assert_eq!(added.len(), 1);
            assert_eq!(added[0].0, b);
            assert_eq!(*added[0].1, 100);
            assert_eq!(changes.changed().count(), 0);
            assert_eq!(changes.removed().count(), 0);
        }
    }

    #[test]
    fn re_read_same_window_is_empty() {
        // After consuming all changes, re-reading should produce empty results.
        let mut world = World::new();

        let a = world.spawn((42,));

        let mut tracker = ChangeTracker::<i32>::new();

        // First track
        {
            let mut changes = tracker.track(&mut world);
            assert_eq!(changes.added().count(), 1);
            assert_eq!(changes.changed().count(), 0);
            assert_eq!(changes.removed().count(), 0);
        }

        // Re-read without any world changes
        {
            let mut changes = tracker.track(&mut world);
            assert_eq!(changes.added().count(), 0);
            assert_eq!(changes.changed().count(), 0);
            assert_eq!(changes.removed().count(), 0);
        }
    }
}

impl<T: Component> Default for ChangeTracker<T> {
    fn default() -> Self {
        Self::new()
    }
}

struct Previous<T>(T);

/// Collection of iterators over changes in `T` components
pub struct Changes<'a, T>
where
    T: Component + Clone + PartialEq,
{
    tracker: &'a mut ChangeTracker<T>,
    world: &'a mut World,
    added: bool,
    changed: bool,
    removed: bool,
}

impl<T> Changes<'_, T>
where
    T: Component + Clone + PartialEq,
{
    /// Iterate over entities which were given a new `T` component after the preceding
    /// [`track`](ChangeTracker::track) call, including newly spawned entities
    pub fn added(&mut self) -> impl ExactSizeIterator<Item = (Entity, &T)> + '_ {
        self.tracker.added_components.clear();
        self.added = true;
        DrainOnDrop::new(
            self.tracker
                .added
                .query_mut(self.world)
                .inspect(|&(e, x)| self.tracker.added_components.push((e, x.clone()))),
        )
    }

    /// Iterate over `(entity, old, new)` for entities whose `T` component has changed according to
    /// [`PartialEq`] after the preceding [`track`](ChangeTracker::track) call
    pub fn changed(&mut self) -> impl Iterator<Item = (Entity, T, &T)> + '_ {
        self.changed = true;
        DrainOnDrop::new(
            self.tracker
                .changed
                .query_mut(self.world)
                .filter_map(|(e, new, old)| {
                    (*new != old.0).then(|| {
                        let old = mem::replace(&mut old.0, new.clone());
                        (e, old, new)
                    })
                }),
        )
    }

    /// Iterate over entities which lost their `T` component after the preceding
    /// [`track`](ChangeTracker::track) call, excluding any entities which were despawned
    pub fn removed(&mut self) -> impl ExactSizeIterator<Item = (Entity, T)> + '_ {
        self.tracker.removed_components.clear();
        self.removed = true;
        // TODO: We could make this much more efficient by introducing a mechanism for queries to
        // take ownership of components directly.
        self.tracker
            .removed_components
            .extend(self.tracker.removed.query_mut(self.world));
        DrainOnDrop::new(
            self.tracker
                .removed_components
                .drain(..)
                .map(|e| (e, self.world.remove_one::<Previous<T>>(e).unwrap().0)),
        )
    }
}

impl<T: Component> Drop for Changes<'_, T>
where
    T: Component + Clone + PartialEq,
{
    fn drop(&mut self) {
        if !self.added {
            _ = self.added();
        }
        for (entity, component) in self.tracker.added_components.drain(..) {
            self.world.insert_one(entity, Previous(component)).unwrap();
        }
        if !self.changed {
            _ = self.changed();
        }
        if !self.removed {
            _ = self.removed();
        }
    }
}

/// Helper to ensure an iterator visits every element so that we can rely on the iterator's side
/// effects
// TODO: remove the Option once Iterator::try_fold can be implemented on stable.
struct DrainOnDrop<T: Iterator>(Option<T>);

impl<T: Iterator> DrainOnDrop<T> {
    fn new(iter: T) -> Self {
        Self(Some(iter))
    }
}

impl<T: Iterator> Iterator for DrainOnDrop<T> {
    type Item = T::Item;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.as_mut()?.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.as_ref().map_or((0, Some(0)), Iterator::size_hint)
    }

    fn fold<B, F>(mut self, init: B, f: F) -> B
    where
        F: FnMut(B, Self::Item) -> B,
    {
        // Forward to the inner iterator so that hecs's per-archetype `fold` fast path is used.
        self.0.take().into_iter().flatten().fold(init, f)
    }
}

impl<T: ExactSizeIterator> ExactSizeIterator for DrainOnDrop<T> {
    fn len(&self) -> usize {
        self.0.as_ref().map_or(0, ExactSizeIterator::len)
    }
}

impl<T: Iterator> Drop for DrainOnDrop<T> {
    fn drop(&mut self) {
        if let Some(iter) = self.0.take() {
            iter.for_each(drop);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke() {
        let mut world = World::new();

        let a = world.spawn((42,));
        let b = world.spawn((17, false));
        let c = world.spawn((true,));

        let mut tracker = ChangeTracker::<i32>::new();
        {
            let mut changes = tracker.track(&mut world);
            let added = changes.added().collect::<Vec<_>>();
            assert_eq!(added.len(), 2);
            assert!(added.contains(&(a, &42)));
            assert!(added.contains(&(b, &17)));
            assert_eq!(changes.changed().count(), 0);
            assert_eq!(changes.removed().count(), 0);
        }

        world.remove_one::<i32>(a).unwrap();
        *world.get::<&mut i32>(b).unwrap() = 26;
        world.insert_one(c, 74).unwrap();
        {
            let mut changes = tracker.track(&mut world);
            assert_eq!(changes.removed().collect::<Vec<_>>(), [(a, 42)]);
            assert_eq!(changes.changed().collect::<Vec<_>>(), [(b, 17, &26)]);
            assert_eq!(changes.added().collect::<Vec<_>>(), [(c, &74)]);
        }
        {
            let mut changes = tracker.track(&mut world);
            assert_eq!(changes.removed().collect::<Vec<_>>(), []);
            assert_eq!(changes.changed().collect::<Vec<_>>(), []);
            assert_eq!(changes.added().collect::<Vec<_>>(), []);
        }
    }

    #[test]
    fn removed_after_component_removal() {
        let mut world = World::new();

        let a = world.spawn((42, true));
        let b = world.spawn((17, false));

        let mut tracker = ChangeTracker::<i32>::new();

        // First track: establish baseline
        {
            let mut changes = tracker.track(&mut world);
            assert_eq!(changes.added().count(), 2);
        }

        // Remove T from entity a
        world.remove_one::<i32>(a).unwrap();

        // Second track: should report a as removed
        {
            let mut changes = tracker.track(&mut world);
            let removed: Vec<_> = changes.removed().collect();
            assert_eq!(removed.len(), 1, "entity a should be reported as removed");
            assert_eq!(removed[0].0, a);
            assert_eq!(removed[0].1, 42);
            assert_eq!(changes.changed().count(), 0);
            assert_eq!(changes.added().count(), 0);
        }
    }

    #[test]
    fn added_after_insert_on_existing_entity() {
        let mut world = World::new();

        let a = world.spawn((true,));

        let mut tracker = ChangeTracker::<i32>::new();

        // First track: no i32 components exist yet
        {
            let mut changes = tracker.track(&mut world);
            assert_eq!(changes.added().count(), 0);
        }

        // Insert i32 into entity a (moves it from {bool} to {bool, i32})
        world.insert_one(a, 42).unwrap();

        // Second track: should report a as added
        {
            let mut changes = tracker.track(&mut world);
            let added: Vec<_> = changes.added().collect();
            assert_eq!(added.len(), 1, "entity a should be reported as added");
            assert_eq!(added[0].0, a);
            assert_eq!(*added[0].1, 42);
        }
    }

    #[test]
    fn change_detection_across_archetype_migration() {
        // Entity has T and is tracked. Then a different component is added,
        // causing the entity to move archetypes. T should still be tracked correctly.
        let mut world = World::new();

        let a = world.spawn((42,));

        let mut tracker = ChangeTracker::<i32>::new();

        // First track: establish baseline
        {
            let mut changes = tracker.track(&mut world);
            assert_eq!(changes.added().count(), 1);
        }

        // Modify T
        *world.get::<&mut i32>(a).unwrap() = 99;

        // Add a different component, causing archetype migration
        world.insert_one(a, true).unwrap();

        // Second track: should detect the change in T
        {
            let mut changes = tracker.track(&mut world);
            let changed: Vec<_> = changes.changed().collect();
            assert_eq!(changed.len(), 1, "entity a should be reported as changed");
            assert_eq!(changed[0].0, a);
            assert_eq!(changed[0].1, 42); // old value
            assert_eq!(*changed[0].2, 99); // new value
        }
    }

    #[test]
    fn same_slot_respawn() {
        // When an entity is despawned and a new one spawns in the same slot,
        // the change tracker should not carry over state from the old entity.
        let mut world = World::new();

        let a = world.spawn((42,));
        let a_id = a.id();
        let a_gen = a.generation;

        let mut tracker = ChangeTracker::<i32>::new();

        // Establish baseline
        {
            let mut changes = tracker.track(&mut world);
            assert_eq!(changes.added().count(), 1);
        }

        // Despawn and respawn in same slot
        world.despawn(a).unwrap();
        let b = world.spawn((100,));
        assert_eq!(b.id(), a_id);
        assert_ne!(b.generation, a_gen);

        // Track: should see b as added with 100, NOT as changed from 42
        {
            let mut changes = tracker.track(&mut world);
            let added: Vec<_> = changes.added().collect();
            assert_eq!(added.len(), 1);
            assert_eq!(added[0].0, b);
            assert_eq!(*added[0].1, 100);
            assert_eq!(changes.changed().count(), 0);
            assert_eq!(changes.removed().count(), 0);
        }
    }

    #[test]
    fn re_read_same_window_is_empty() {
        // After consuming all changes, re-reading should produce empty results.
        let mut world = World::new();

        let a = world.spawn((42,));

        let mut tracker = ChangeTracker::<i32>::new();

        // First track
        {
            let mut changes = tracker.track(&mut world);
            assert_eq!(changes.added().count(), 1);
            assert_eq!(changes.changed().count(), 0);
            assert_eq!(changes.removed().count(), 0);
        }

        // Re-read without any world changes
        {
            let mut changes = tracker.track(&mut world);
            assert_eq!(changes.added().count(), 0);
            assert_eq!(changes.changed().count(), 0);
            assert_eq!(changes.removed().count(), 0);
        }
    }
}
