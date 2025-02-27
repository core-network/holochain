#![cfg(test)]
#![cfg(feature = "test_utils")]

use super::*;

use crate::core::queue_consumer::TriggerSender;
use crate::here;
use ::fixt::prelude::*;
use holochain_sqlite::db::WriteManager;
use holochain_state::query::link::GetLinksQuery;
use holochain_state::workspace::WorkspaceError;
use holochain_zome_types::Entry;
use holochain_zome_types::HeaderHashed;
use holochain_zome_types::ValidationStatus;
use observability;

#[derive(Clone)]
struct TestData {
    signature: Signature,
    original_entry: Entry,
    new_entry: Entry,
    any_header: Header,
    dna_header: Header,
    entry_update_header: Update,
    entry_update_entry: Update,
    original_header_hash: HeaderHash,
    original_entry_hash: EntryHash,
    new_entry_hash: EntryHash,
    original_header: NewEntryHeader,
    entry_delete: Delete,
    link_add: CreateLink,
    link_remove: DeleteLink,
}

impl TestData {
    async fn new() -> Self {
        // original entry
        let original_entry = EntryFixturator::new(AppEntry).next().unwrap();
        // New entry
        let new_entry = EntryFixturator::new(AppEntry).next().unwrap();
        Self::new_inner(original_entry, new_entry)
    }

    #[instrument()]
    fn new_inner(original_entry: Entry, new_entry: Entry) -> Self {
        // original entry
        let original_entry_hash =
            EntryHashed::from_content_sync(original_entry.clone()).into_hash();

        // New entry
        let new_entry_hash = EntryHashed::from_content_sync(new_entry.clone()).into_hash();

        // Original entry and header for updates
        let mut original_header = fixt!(NewEntryHeader, PublicCurve);
        debug!(?original_header);

        match &mut original_header {
            NewEntryHeader::Create(c) => c.entry_hash = original_entry_hash.clone(),
            NewEntryHeader::Update(u) => u.entry_hash = original_entry_hash.clone(),
        }

        let original_header_hash =
            HeaderHashed::from_content_sync(original_header.clone().into()).into_hash();

        // Header for the new entry
        let mut new_entry_header = fixt!(NewEntryHeader, PublicCurve);

        // Update to new entry
        match &mut new_entry_header {
            NewEntryHeader::Create(c) => c.entry_hash = new_entry_hash.clone(),
            NewEntryHeader::Update(u) => u.entry_hash = new_entry_hash.clone(),
        }

        // Entry update for header
        let mut entry_update_header = fixt!(Update, PublicCurve);
        entry_update_header.entry_hash = new_entry_hash.clone();
        entry_update_header.original_header_address = original_header_hash.clone();

        // Entry update for entry
        let mut entry_update_entry = fixt!(Update, PublicCurve);
        entry_update_entry.entry_hash = new_entry_hash.clone();
        entry_update_entry.original_entry_address = original_entry_hash.clone();
        entry_update_entry.original_header_address = original_header_hash.clone();

        // Entry delete
        let mut entry_delete = fixt!(Delete);
        entry_delete.deletes_address = original_header_hash.clone();

        // Link add
        let mut link_add = fixt!(CreateLink);
        link_add.base_address = original_entry_hash.clone();
        link_add.target_address = new_entry_hash.clone();
        link_add.zome_id = fixt!(ZomeId);
        link_add.tag = fixt!(LinkTag);

        let link_add_hash = HeaderHashed::from_content_sync(link_add.clone().into()).into_hash();

        // Link remove
        let mut link_remove = fixt!(DeleteLink);
        link_remove.base_address = original_entry_hash.clone();
        link_remove.link_add_address = link_add_hash.clone();

        // Any Header
        let mut any_header = fixt!(Header, PublicCurve);
        match &mut any_header {
            Header::Create(ec) => {
                ec.entry_hash = original_entry_hash.clone();
            }
            Header::Update(eu) => {
                eu.entry_hash = original_entry_hash.clone();
            }
            _ => {}
        };

        // Dna Header
        let dna_header = Header::Dna(fixt!(Dna));

        Self {
            signature: fixt!(Signature),
            original_entry,
            new_entry,
            any_header,
            dna_header,
            entry_update_header,
            entry_update_entry,
            original_header,
            original_header_hash,
            original_entry_hash,
            entry_delete,
            link_add,
            link_remove,
            new_entry_hash,
        }
    }
}

#[derive(Clone)]
enum Db {
    Integrated(DhtOp),
    IntegratedEmpty,
    IntQueue(DhtOp),
    IntQueueEmpty,
    CasHeader(Header, Option<Signature>),
    CasEntry(Entry, Option<Header>, Option<Signature>),
    MetaEmpty,
    MetaHeader(Entry, Header),
    MetaActivity(Header),
    MetaUpdate(AnyDhtHash, Header),
    MetaDelete(HeaderHash, Header),
    MetaLink(CreateLink, EntryHash),
    MetaLinkEmpty(CreateLink),
}

impl Db {
    /// Checks that the database is in a state
    #[instrument(skip(expects, env))]
    async fn check(expects: Vec<Self>, env: EnvWrite, here: String) {
        fresh_reader_test(env, |txn| {
            // print_stmts_test(env, |txn| {
            for expect in expects {
                match expect {
                    Db::Integrated(op) => {
                        let op_hash = DhtOpHash::with_data_sync(&op);

                        let found: bool = txn
                            .query_row(
                                "
                                SELECT EXISTS(
                                    SELECT 1 FROM DhtOP 
                                    WHERE when_integrated IS NOT NULL 
                                    AND hash = :hash
                                    AND validation_status = :status
                                )
                                ",
                                named_params! {
                                    ":hash": op_hash,
                                    ":status": ValidationStatus::Valid,
                                },
                                |row| row.get(0),
                            )
                            .unwrap();
                        assert!(found, "{}\n{:?}", here, op);
                    }
                    Db::IntQueue(op) => {
                        let op_hash = DhtOpHash::with_data_sync(&op);

                        let found: bool = txn
                            .query_row(
                                "
                                SELECT EXISTS(
                                    SELECT 1 FROM DhtOP 
                                    WHERE when_integrated IS NULL 
                                    AND validation_stage = 3
                                    AND hash = :hash
                                    AND validation_status = :status
                                )
                                ",
                                named_params! {
                                    ":hash": op_hash,
                                    ":status": ValidationStatus::Valid,
                                },
                                |row| row.get(0),
                            )
                            .unwrap();
                        assert!(found, "{}\n{:?}", here, op);
                    }
                    Db::CasHeader(header, _) => {
                        let hash = HeaderHash::with_data_sync(&header);
                        let found: bool = txn
                            .query_row(
                                "
                                SELECT EXISTS(
                                    SELECT 1 FROM DhtOP 
                                    WHERE when_integrated IS NOT NULL 
                                    AND header_hash = :hash
                                    AND validation_status = :status
                                    AND (type = :store_entry OR type = :store_element)
                                )
                                ",
                                named_params! {
                                    ":hash": hash,
                                    ":status": ValidationStatus::Valid,
                                    ":store_entry": DhtOpType::StoreEntry,
                                    ":store_element": DhtOpType::StoreElement,
                                },
                                |row| row.get(0),
                            )
                            .unwrap();
                        assert!(found, "{}\n{:?}", here, header);
                    }
                    Db::CasEntry(entry, _, _) => {
                        let hash = EntryHash::with_data_sync(&entry);
                        let basis: AnyDhtHash = hash.clone().into();
                        let found: bool = txn
                            .query_row(
                                "
                                SELECT EXISTS(
                                    SELECT 1 FROM DhtOp
                                    JOIN Header ON DhtOp.header_hash = Header.hash
                                    WHERE DhtOp.when_integrated IS NOT NULL 
                                    AND DhtOp.validation_status = :status
                                    AND (
                                        (Header.entry_hash = :hash AND DhtOp.type = :store_element)
                                        OR
                                        (DhtOp.basis_hash = :basis AND DhtOp.type = :store_entry)
                                    )
                                )
                                ",
                                named_params! {
                                    ":hash": hash,
                                    ":basis": basis,
                                    ":status": ValidationStatus::Valid,
                                    ":store_entry": DhtOpType::StoreEntry,
                                    ":store_element": DhtOpType::StoreElement,
                                },
                                |row| row.get(0),
                            )
                            .unwrap();
                        assert!(found, "{}\n{:?}", here, entry);
                    }
                    Db::MetaHeader(entry, header) => {
                        let hash = HeaderHash::with_data_sync(&header);
                        let basis: AnyDhtHash = EntryHash::with_data_sync(&entry).into();
                        let found: bool = txn
                            .query_row(
                                "
                                SELECT EXISTS(
                                    SELECT 1 FROM DhtOP 
                                    WHERE when_integrated IS NOT NULL 
                                    AND basis_hash = :basis
                                    AND header_hash = :hash
                                    AND validation_status = :status
                                    AND type = :store_entry
                                )
                                ",
                                named_params! {
                                    ":basis": basis,
                                    ":hash": hash,
                                    ":status": ValidationStatus::Valid,
                                    ":store_entry": DhtOpType::StoreEntry,
                                },
                                |row| row.get(0),
                            )
                            .unwrap();
                        assert!(found, "{}\n{:?}", here, entry);
                    }
                    Db::MetaActivity(header) => {
                        let hash = HeaderHash::with_data_sync(&header);
                        let basis: AnyDhtHash = header.author().clone().into();
                        let found: bool = txn
                            .query_row(
                                "
                                SELECT EXISTS(
                                    SELECT 1 FROM DhtOP 
                                    WHERE when_integrated IS NOT NULL 
                                    AND basis_hash = :basis
                                    AND header_hash = :hash
                                    AND validation_status = :status
                                    AND type = :activity
                                )
                                ",
                                named_params! {
                                    ":basis": basis,
                                    ":hash": hash,
                                    ":status": ValidationStatus::Valid,
                                    ":activity": DhtOpType::RegisterAgentActivity,
                                },
                                |row| row.get(0),
                            )
                            .unwrap();
                        assert!(found, "{}\n{:?}", here, header);
                    }
                    Db::MetaUpdate(base, header) => {
                        let hash = HeaderHash::with_data_sync(&header);
                        let found: bool = txn
                            .query_row(
                                "
                                SELECT EXISTS(
                                    SELECT 1 FROM DhtOP 
                                    WHERE when_integrated IS NOT NULL 
                                    AND basis_hash = :basis
                                    AND header_hash = :hash
                                    AND validation_status = :status
                                    AND (type = :update_content OR type = :update_element)
                                )
                                ",
                                named_params! {
                                    ":basis": base,
                                    ":hash": hash,
                                    ":status": ValidationStatus::Valid,
                                    ":update_content": DhtOpType::RegisterUpdatedContent,
                                    ":update_element": DhtOpType::RegisterUpdatedElement,
                                },
                                |row| row.get(0),
                            )
                            .unwrap();
                        assert!(found, "{}\n{:?}", here, header);
                    }
                    Db::MetaDelete(deleted_header_hash, header) => {
                        let hash = HeaderHash::with_data_sync(&header);
                        let found: bool = txn
                            .query_row(
                                "
                                SELECT EXISTS(
                                    SELECT 1 FROM DhtOP
                                    JOIN Header on DhtOp.header_hash = Header.hash
                                    WHERE when_integrated IS NOT NULL 
                                    AND validation_status = :status
                                    AND (
                                        (DhtOp.type = :deleted_entry_header AND Header.deletes_header_hash = :deleted_header_hash)
                                        OR 
                                        (DhtOp.type = :deleted_by AND header_hash = :hash)
                                    )
                                )
                                ",
                                named_params! {
                                    ":deleted_header_hash": deleted_header_hash,
                                    ":hash": hash,
                                    ":status": ValidationStatus::Valid,
                                    ":deleted_by": DhtOpType::RegisterDeletedBy,
                                    ":deleted_entry_header": DhtOpType::RegisterDeletedEntryHeader,
                                },
                                |row| row.get(0),
                            )
                            .unwrap();
                        assert!(found, "{}\n{:?}", here, header);
                    }
                    Db::IntegratedEmpty => {
                        let not_empty: bool = txn
                            .query_row(
                                "SELECT EXISTS(SELECT 1 FROM DhtOP WHERE when_integrated IS NOT NULL)",
                                [],
                                |row| row.get(0),
                            )
                            .unwrap();
                        assert!(!not_empty, "{}", here);
                    }
                    Db::IntQueueEmpty => {
                        let not_empty: bool = txn
                            .query_row(
                                "SELECT EXISTS(SELECT 1 FROM DhtOP WHERE when_integrated IS NULL)",
                                [],
                                |row| row.get(0),
                            )
                            .unwrap();
                        assert!(!not_empty, "{}", here);
                    }
                    Db::MetaEmpty => {
                        let not_empty: bool = txn
                            .query_row(
                                "SELECT EXISTS(SELECT 1 FROM DhtOP WHERE when_integrated IS NOT NULL)",
                                [],
                                |row| row.get(0),
                            )
                            .unwrap();
                        assert!(!not_empty, "{}", here);
                    }
                    Db::MetaLink(link_add, target_hash) => {
                        let link_add_hash =
                            HeaderHash::with_data_sync(&Header::from(link_add.clone()));
                        let query = GetLinksQuery::new(
                            link_add.base_address.clone(),
                            link_add.zome_id,
                            Some(link_add.tag.clone()),
                        );
                        let res = query.run(Txn::from(&txn)).unwrap();
                        assert_eq!(res.len(), 1, "{}", here);
                        assert_eq!(res[0].create_link_hash, link_add_hash, "{}", here);
                        assert_eq!(res[0].target, target_hash, "{}", here);
                        assert_eq!(res[0].tag, link_add.tag, "{}", here);
                    }
                    Db::MetaLinkEmpty(link_add) => {
                        let query = GetLinksQuery::new(
                            link_add.base_address.clone(),
                            link_add.zome_id,
                            Some(link_add.tag.clone()),
                        );
                        let res = query.run(Txn::from(&txn)).unwrap();
                        assert_eq!(res.len(), 0, "{}", here);
                    }
                }
            }
        })
    }

    // Sets the database to a certain state
    #[instrument(skip(pre_state, env))]
    async fn set<'env>(pre_state: Vec<Self>, env: EnvWrite) {
        env.conn()
            .unwrap()
            .with_commit_sync::<WorkspaceError, _, _>(|txn| {
                for state in pre_state {
                    match state {
                        Db::Integrated(op) => {
                            let op = DhtOpHashed::from_content_sync(op.clone());
                            let hash = op.as_hash().clone();
                            mutations::insert_op(txn, op, false).unwrap();
                            mutations::set_when_integrated(txn, hash.clone(), timestamp::now())
                                .unwrap();
                            mutations::set_validation_status(txn, hash, ValidationStatus::Valid)
                                .unwrap();
                        }
                        Db::IntQueue(op) => {
                            let op = DhtOpHashed::from_content_sync(op.clone());
                            let hash = op.as_hash().clone();
                            mutations::insert_op(txn, op, false).unwrap();
                            mutations::set_validation_stage(
                                txn,
                                hash.clone(),
                                ValidationLimboStatus::AwaitingIntegration,
                            )
                            .unwrap();
                            mutations::set_validation_status(txn, hash, ValidationStatus::Valid)
                                .unwrap();
                        }
                        _ => {
                            unimplemented!("Use Db::Integrated");
                        }
                    }
                }
                Ok(())
            })
            .unwrap();
    }
}

async fn call_workflow<'env>(env: EnvWrite) {
    let (qt, _rx) = TriggerSender::new();
    let (qt2, _rx) = TriggerSender::new();
    integrate_dht_ops_workflow(env.clone(), qt, qt2)
        .await
        .unwrap();
}

// Need to clear the data from the previous test
fn clear_dbs(env: EnvWrite) {
    env.conn()
        .unwrap()
        .with_commit_sync(|txn| {
            txn.execute("DELETE FROM DhtOP", []).unwrap();
            txn.execute("DELETE FROM Header", []).unwrap();
            txn.execute("DELETE FROM Entry", []).unwrap();
            StateMutationResult::Ok(())
        })
        .unwrap();
}

// TESTS BEGIN HERE
// The following show an op or ops that you want to test
// with a desired pre-state that you want the database in
// and the expected state of the database after the workflow is run

fn store_element(a: TestData) -> (Vec<Db>, Vec<Db>, &'static str) {
    let entry = match &a.any_header {
        Header::Create(_) | Header::Update(_) => Some(a.original_entry.clone().into()),
        _ => None,
    };
    let op = DhtOp::StoreElement(
        a.signature.clone(),
        a.any_header.clone().into(),
        entry.clone(),
    );
    let pre_state = vec![Db::IntQueue(op.clone())];
    // Add op data to pending
    let mut expect = vec![
        Db::Integrated(op.clone()),
        Db::CasHeader(a.any_header.clone().into(), None),
    ];
    if let Some(_) = &entry {
        expect.push(Db::CasEntry(a.original_entry.clone(), None, None));
    }
    (pre_state, expect, "store element")
}

fn store_entry(a: TestData) -> (Vec<Db>, Vec<Db>, &'static str) {
    let op = DhtOp::StoreEntry(
        a.signature.clone(),
        a.original_header.clone(),
        a.original_entry.clone().into(),
    );
    debug!(?a.original_header);
    let pre_state = vec![Db::IntQueue(op.clone())];
    let expect = vec![
        Db::Integrated(op.clone()),
        Db::CasHeader(a.original_header.clone().into(), None),
        Db::CasEntry(a.original_entry.clone(), None, None),
        Db::MetaHeader(a.original_entry.clone(), a.original_header.clone().into()),
    ];
    (pre_state, expect, "store entry")
}

fn register_agent_activity(a: TestData) -> (Vec<Db>, Vec<Db>, &'static str) {
    let op = DhtOp::RegisterAgentActivity(a.signature.clone(), a.dna_header.clone());
    let pre_state = vec![Db::IntQueue(op.clone())];
    let expect = vec![
        Db::Integrated(op.clone()),
        Db::MetaActivity(a.dna_header.clone()),
    ];
    (pre_state, expect, "register agent activity")
}

fn register_updated_element(a: TestData) -> (Vec<Db>, Vec<Db>, &'static str) {
    let original_op = DhtOp::StoreElement(
        a.signature.clone(),
        a.original_header.clone().into(),
        Some(a.original_entry.clone().into()),
    );
    let op = DhtOp::RegisterUpdatedElement(
        a.signature.clone(),
        a.entry_update_header.clone(),
        Some(a.new_entry.clone().into()),
    );
    let pre_state = vec![Db::Integrated(original_op), Db::IntQueue(op.clone())];
    let expect = vec![
        Db::Integrated(op.clone()),
        Db::MetaUpdate(
            a.original_header_hash.clone().into(),
            a.entry_update_header.clone().into(),
        ),
    ];
    (pre_state, expect, "register updated element")
}

fn register_replaced_by_for_entry(a: TestData) -> (Vec<Db>, Vec<Db>, &'static str) {
    let original_op = DhtOp::StoreEntry(
        a.signature.clone(),
        a.original_header.clone(),
        a.original_entry.clone().into(),
    );
    let op = DhtOp::RegisterUpdatedContent(
        a.signature.clone(),
        a.entry_update_entry.clone(),
        Some(a.new_entry.clone().into()),
    );
    let pre_state = vec![Db::Integrated(original_op), Db::IntQueue(op.clone())];
    let expect = vec![
        Db::Integrated(op.clone()),
        Db::MetaUpdate(
            a.original_entry_hash.clone().into(),
            a.entry_update_entry.clone().into(),
        ),
    ];
    (pre_state, expect, "register replaced by for entry")
}

fn register_deleted_by(a: TestData) -> (Vec<Db>, Vec<Db>, &'static str) {
    let original_op = DhtOp::StoreEntry(
        a.signature.clone(),
        a.original_header.clone(),
        a.original_entry.clone().into(),
    );
    let op = DhtOp::RegisterDeletedEntryHeader(a.signature.clone(), a.entry_delete.clone());
    let pre_state = vec![Db::Integrated(original_op), Db::IntQueue(op.clone())];
    let expect = vec![
        Db::IntQueueEmpty,
        Db::Integrated(op.clone()),
        Db::MetaDelete(
            a.original_header_hash.clone().into(),
            a.entry_delete.clone().into(),
        ),
    ];
    (pre_state, expect, "register deleted by")
}

fn register_deleted_header_by(a: TestData) -> (Vec<Db>, Vec<Db>, &'static str) {
    let original_op = DhtOp::StoreElement(
        a.signature.clone(),
        a.original_header.clone().into(),
        Some(a.original_entry.clone().into()),
    );
    let op = DhtOp::RegisterDeletedBy(a.signature.clone(), a.entry_delete.clone());
    let pre_state = vec![Db::IntQueue(op.clone()), Db::Integrated(original_op)];
    let expect = vec![
        Db::Integrated(op.clone()),
        Db::MetaDelete(
            a.original_header_hash.clone().into(),
            a.entry_delete.clone().into(),
        ),
    ];
    (pre_state, expect, "register deleted header by")
}

fn register_add_link(a: TestData) -> (Vec<Db>, Vec<Db>, &'static str) {
    let original_op = DhtOp::StoreEntry(
        a.signature.clone(),
        a.original_header.clone(),
        a.original_entry.clone().into(),
    );
    let op = DhtOp::RegisterAddLink(a.signature.clone(), a.link_add.clone());
    let pre_state = vec![Db::Integrated(original_op), Db::IntQueue(op.clone())];
    let expect = vec![
        Db::Integrated(op.clone()),
        Db::MetaLink(a.link_add.clone(), a.new_entry_hash.clone().into()),
    ];
    (pre_state, expect, "register link add")
}

fn register_delete_link(a: TestData) -> (Vec<Db>, Vec<Db>, &'static str) {
    let original_op = DhtOp::StoreEntry(
        a.signature.clone(),
        a.original_header.clone(),
        a.original_entry.clone().into(),
    );
    let original_link_op = DhtOp::RegisterAddLink(a.signature.clone(), a.link_add.clone());
    let op = DhtOp::RegisterRemoveLink(a.signature.clone(), a.link_remove.clone());
    let pre_state = vec![
        Db::Integrated(original_op),
        Db::Integrated(original_link_op),
        Db::IntQueue(op.clone()),
    ];
    let expect = vec![
        Db::Integrated(op.clone()),
        Db::MetaLinkEmpty(a.link_add.clone()),
    ];
    (pre_state, expect, "register link remove")
}

// Link remove when not an author
fn register_delete_link_missing_base(a: TestData) -> (Vec<Db>, Vec<Db>, &'static str) {
    let op = DhtOp::RegisterRemoveLink(a.signature.clone(), a.link_remove.clone());
    let pre_state = vec![Db::IntQueue(op.clone())];
    let expect = vec![Db::IntegratedEmpty, Db::IntQueue(op.clone()), Db::MetaEmpty];
    (
        pre_state,
        expect,
        "register remove link remove missing base",
    )
}

// This runs the above tests
#[tokio::test(flavor = "multi_thread")]
async fn test_ops_state() {
    observability::test_run().ok();
    let test_env = test_cell_env();
    let env = test_env.env();

    let tests = [
        store_element,
        store_entry,
        register_agent_activity,
        register_replaced_by_for_entry,
        register_updated_element,
        register_deleted_by,
        register_deleted_header_by,
        register_add_link,
        register_delete_link,
        register_delete_link_missing_base,
    ];

    for t in tests.iter() {
        clear_dbs(env.clone());
        println!("test_ops_state on function {:?}", t);
        let td = TestData::new().await;
        let (pre_state, expect, name) = t(td);
        Db::set(pre_state, env.clone()).await;
        call_workflow(env.clone()).await;
        Db::check(expect, env.clone(), format!("{}: {}", name, here!(""))).await;
    }
}

#[cfg(todo_redo_old_tests)]
async fn commit_entry<'env>(
    pre_state: Vec<Db>,
    env: EnvWrite,
    zome_name: ZomeName,
) -> (EntryHash, HeaderHash) {
    let workspace = CallZomeWorkspace::new(env.clone().into()).unwrap();
    let workspace_lock = CallZomeWorkspaceLock::new(workspace);

    // Create entry def with the correct zome name
    let entry_def_id = fixt!(EntryDefId);
    let mut entry_def = fixt!(EntryDef);
    entry_def.id = entry_def_id.clone();
    let mut entry_defs_map = BTreeMap::new();
    entry_defs_map.insert(
        ZomeName::from(zome_name.clone()),
        EntryDefs::from(vec![entry_def]),
    );

    // Create a dna file with the correct zome name in the desired position (ZomeId)
    let dna_file = DnaFileFixturator::new(Empty).next().unwrap();
    let mut dna_def = dna_file.dna_def().clone();
    let zome = Zome::new(zome_name.clone().into(), fixt!(ZomeDef));
    dna_def.zomes.clear();
    dna_def.zomes.push(zome.clone().into());
    let dna_def = DnaDefHashed::from_content_sync(dna_def);

    // Create ribosome mock to return fixtures
    // This is a lot faster then compiling a zome
    let mut ribosome = MockRibosomeT::new();
    ribosome.expect_dna_def().return_const(dna_def);
    ribosome
        .expect_zome_to_id()
        .returning(|_| Ok(ZomeId::from(1)));

    ribosome
        .expect_run_entry_defs()
        .returning(move |_, _| Ok(EntryDefsResult::Defs(entry_defs_map.clone())));

    let mut call_context = CallContextFixturator::new(Unpredictable).next().unwrap();
    call_context.zome = zome.clone();

    // Collect the entry from the pre-state to commit
    let entry = pre_state
        .into_iter()
        .filter_map(|state| match state {
            Db::IntQueue(_) => {
                // Will be provided by triggering the produce workflow
                None
            }
            Db::CasEntry(entry, _, _) => Some(entry),
            _ => {
                unreachable!("This test only needs integration queue and an entry in the elements")
            }
        })
        .next()
        .unwrap();

    let input = EntryWithDefId::new(entry_def_id.clone(), entry.clone());

    let output = {
        let mut host_access = fixt!(ZomeCallHostAccess);
        host_access.workspace = workspace_lock.clone();
        call_context.host_access = host_access.into();
        let ribosome = Arc::new(ribosome);
        let call_context = Arc::new(call_context);
        host_fn::create::create(ribosome.clone(), call_context.clone(), input).unwrap()
    };

    // Write
    {
        let mut workspace = workspace_lock.write().await;
        env.conn()
            .unwrap()
            .with_commit(|writer| workspace.flush_to_txn_ref(writer))
            .unwrap();
    }

    let entry_hash = holochain_types::entry::EntryHashed::from_content_sync(entry).into_hash();

    (entry_hash, output)
}

#[cfg(todo_redo_old_tests)]
async fn get_entry(env: EnvWrite, entry_hash: EntryHash) -> Option<Entry> {
    let workspace = CallZomeWorkspace::new(env.clone().into()).unwrap();
    let workspace_lock = CallZomeWorkspaceLock::new(workspace);

    // Create ribosome mock to return fixtures
    // This is a lot faster then compiling a zome
    let ribosome = MockRibosomeT::new();

    let mut call_context = CallContextFixturator::new(Unpredictable).next().unwrap();

    let input = GetInput::new(entry_hash.clone().into(), GetOptions::latest());

    let output = {
        let mut host_access = fixt!(ZomeCallHostAccess);
        host_access.workspace = workspace_lock;
        call_context.host_access = host_access.into();
        let ribosome = Arc::new(ribosome);
        let call_context = Arc::new(call_context);
        host_fn::get::get(ribosome.clone(), call_context.clone(), input).unwrap()
    };
    output.and_then(|el| el.into())
}

#[cfg(todo_redo_old_tests)]
async fn create_link(
    env: EnvWrite,
    base_address: EntryHash,
    target_address: EntryHash,
    zome_name: ZomeName,
    link_tag: LinkTag,
) -> HeaderHash {
    let workspace = CallZomeWorkspace::new(env.clone().into()).unwrap();
    let workspace_lock = CallZomeWorkspaceLock::new(workspace);

    // Create a dna file with the correct zome name in the desired position (ZomeId)
    let dna_file = DnaFileFixturator::new(Empty).next().unwrap();
    let mut dna_def = dna_file.dna_def().clone();
    let zome = Zome::new(zome_name.clone().into(), fixt!(ZomeDef));
    dna_def.zomes.clear();
    dna_def.zomes.push(zome.clone().into());
    let dna_def = DnaDefHashed::from_content_sync(dna_def);

    // Create ribosome mock to return fixtures
    // This is a lot faster then compiling a zome
    let mut ribosome = MockRibosomeT::new();
    ribosome.expect_dna_def().return_const(dna_def);
    ribosome
        .expect_zome_to_id()
        .returning(|_| Ok(ZomeId::from(1)));

    let mut call_context = CallContextFixturator::new(Unpredictable).next().unwrap();
    call_context.zome = zome.clone();

    // Call create_link
    let input = CreateLinkInput::new(base_address.into(), target_address.into(), link_tag);

    let output = {
        let mut host_access = fixt!(ZomeCallHostAccess);
        host_access.workspace = workspace_lock.clone();
        call_context.host_access = host_access.into();
        let ribosome = Arc::new(ribosome);
        let call_context = Arc::new(call_context);
        // Call the real create_link host fn
        host_fn::create_link::create_link(ribosome.clone(), call_context.clone(), input).unwrap()
    };

    // Write the changes
    {
        let mut workspace = workspace_lock.write().await;
        env.conn()
            .unwrap()
            .with_commit(|writer| workspace.flush_to_txn_ref(writer))
            .unwrap();
    }

    // Get the CreateLink HeaderHash back
    output
}

#[cfg(todo_redo_old_tests)]
async fn get_links(
    env: EnvWrite,
    base_address: EntryHash,
    zome_name: ZomeName,
    link_tag: LinkTag,
) -> Links {
    let workspace = CallZomeWorkspace::new(env.clone().into()).unwrap();
    let workspace_lock = CallZomeWorkspaceLock::new(workspace);

    // Create a dna file with the correct zome name in the desired position (ZomeId)
    let dna_file = DnaFileFixturator::new(Empty).next().unwrap();
    let mut dna_def = dna_file.dna_def().clone();
    let zome = Zome::new(zome_name.clone().into(), fixt!(ZomeDef));
    dna_def.zomes.clear();
    dna_def.zomes.push(zome.clone().into());
    let dna_def = DnaDefHashed::from_content_sync(dna_def);

    let test_network = test_network(Some(dna_def.as_hash().clone()), None).await;

    // Create ribosome mock to return fixtures
    // This is a lot faster then compiling a zome
    let mut ribosome = MockRibosomeT::new();
    ribosome.expect_dna_def().return_const(dna_def);
    ribosome
        .expect_zome_to_id()
        .returning(|_| Ok(ZomeId::from(1)));

    let mut call_context = CallContextFixturator::new(Unpredictable).next().unwrap();
    call_context.zome = zome.clone();

    // Call get links
    let input = GetLinksInput::new(base_address.into(), Some(link_tag));

    let mut host_access = fixt!(ZomeCallHostAccess);
    host_access.workspace = workspace_lock;
    host_access.network = test_network.cell_network();
    call_context.host_access = host_access.into();
    let ribosome = Arc::new(ribosome);
    let call_context = Arc::new(call_context);
    host_fn::get_links::get_links(ribosome.clone(), call_context.clone(), input).unwrap()
}

// This test is designed to run like the
// register_add_link test except all the
// pre-state is added through real host fn calls
#[tokio::test(flavor = "multi_thread")]
#[cfg(todo_redo_old_tests)]
async fn test_metadata_from_wasm_api() {
    // test workspace boilerplate
    observability::test_run().ok();
    let test_env = holochain_state::test_utils::test_cell_env();
    let env = test_env.env();
    clear_dbs(env.clone());

    // Generate fixture data
    let mut td = TestData::with_app_entry_type().await;
    // Only one zome in this test
    td.link_add.zome_id = 0.into();
    let link_tag = td.link_add.tag.clone();
    let target_entry_hash = td.new_entry_hash.clone();
    let zome_name = fixt!(ZomeName);

    // Get db states for an add link op
    let (pre_state, _expect, _) = register_add_link(td);

    // Setup the source chain
    genesis(env.clone()).await;

    // Commit the base
    let base_address = commit_entry(pre_state, env.clone(), zome_name.clone())
        .await
        .0;

    // Link the base to the target
    let _link_add_address = create_link(
        env.clone(),
        base_address.clone(),
        target_entry_hash.clone(),
        zome_name.clone(),
        link_tag.clone(),
    )
    .await;

    // Trigger the produce workflow
    produce_dht_ops(env.clone()).await;

    // Call integrate
    call_workflow(env.clone()).await;

    // Call get links and get back the targets
    let links = get_links(env.clone(), base_address, zome_name, link_tag).await;
    let links = links
        .into_inner()
        .into_iter()
        .map(|h| h.target.try_into().unwrap())
        .collect::<Vec<EntryHash>>();

    // Check we only go a single link
    assert_eq!(links.len(), 1);
    // Check we got correct target_entry_hash
    assert_eq!(links[0], target_entry_hash);
    // TODO: create the expect from the result of the commit and link entries
    // Db::check(
    //     expect,
    //     &env.conn().unwrap(),
    //     &dbs,
    //     format!("{}: {}", "metadata from wasm", here!("")),
    // )
    // .await;
}

// This doesn't work without inline integration
#[tokio::test(flavor = "multi_thread")]
#[cfg(todo_redo_old_tests)]
async fn test_wasm_api_without_integration_links() {
    // test workspace boilerplate
    observability::test_run().ok();
    let test_env = holochain_state::test_utils::test_cell_env();
    let env = test_env.env();
    clear_dbs(env.clone());

    // Generate fixture data
    let mut td = TestData::with_app_entry_type().await;
    // Only one zome in this test
    td.link_add.zome_id = 0.into();
    let link_tag = td.link_add.tag.clone();
    let target_entry_hash = td.new_entry_hash.clone();
    let zome_name = fixt!(ZomeName);

    // Get db states for an add link op
    let (pre_state, _expect, _) = register_add_link(td);

    // Setup the source chain
    genesis(env.clone()).await;

    // Commit the base
    let base_address = commit_entry(pre_state, env.clone(), zome_name.clone())
        .await
        .0;

    // Link the base to the target
    let _link_add_address = create_link(
        env.clone(),
        base_address.clone(),
        target_entry_hash.clone(),
        zome_name.clone(),
        link_tag.clone(),
    )
    .await;

    // Call get links and get back the targets
    let links = get_links(env.clone(), base_address, zome_name, link_tag).await;
    let links = links
        .into_inner()
        .into_iter()
        .map(|h| h.target.try_into().unwrap())
        .collect::<Vec<EntryHash>>();

    // Check we only go a single link
    assert_eq!(links.len(), 1);
    // Check we got correct target_entry_hash
    assert_eq!(links[0], target_entry_hash);
}

#[cfg(todo_redo_old_tests)]
#[ignore = "Evaluate if this test adds any value or remove"]
#[tokio::test(flavor = "multi_thread")]
async fn test_wasm_api_without_integration_delete() {
    // test workspace boilerplate
    observability::test_run().ok();
    let test_env = holochain_state::test_utils::test_cell_env();
    let env = test_env.env();
    clear_dbs(env.clone());

    // Generate fixture data
    let mut td = TestData::with_app_entry_type().await;
    // Only one zome in this test
    td.link_add.zome_id = 0.into();
    let original_entry = td.original_entry.clone();
    let zome_name = fixt!(ZomeName);

    // Get db states for an add link op
    let (pre_state, _expect, _) = register_add_link(td.clone());

    // Setup the source chain
    genesis(env.clone()).await;

    // Commit the base
    let base_address = commit_entry(pre_state.clone(), env.clone(), zome_name.clone())
        .await
        .0;

    // Trigger the produce workflow
    produce_dht_ops(env.clone()).await;

    // Call integrate
    call_workflow(env.clone()).await;

    {
        let mut workspace = CallZomeWorkspace::new(env.clone().into()).unwrap();
        let entry_header = fresh_reader_test!(env, |mut reader| workspace
            .meta_authored
            .get_headers(&mut reader, base_address.clone())
            .unwrap()
            .next()
            .unwrap()
            .unwrap());
        let delete = builder::Delete {
            deletes_address: entry_header.header_hash,
            deletes_entry_address: base_address.clone(),
        };
        workspace.source_chain.put(delete, None).await.unwrap();
        env.conn()
            .unwrap()
            .with_commit(|writer| workspace.flush_to_txn(writer))
            .unwrap();
    }
    // Trigger the produce workflow
    produce_dht_ops(env.clone()).await;

    // Call integrate
    call_workflow(env.clone()).await;
    assert_eq!(get_entry(env.clone(), base_address.clone()).await, None);
    let base_address = commit_entry(pre_state, env.clone(), zome_name.clone())
        .await
        .0;
    assert_eq!(
        get_entry(env.clone(), base_address.clone()).await,
        Some(original_entry)
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "write this test"]
async fn test_integrate_single_register_replaced_by_for_header() {
    // For RegisterUpdatedContent with intended_for Header
    // metadata has Update on HeaderHash but not EntryHash
    todo!("write this test")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "write this test"]
async fn test_integrate_single_register_replaced_by_for_entry() {
    // For RegisterUpdatedContent with intended_for Entry
    // metadata has Update on EntryHash but not HeaderHash
    todo!("write this test")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "write this test"]
async fn test_integrate_single_register_delete_on_headerd_by() {
    // For RegisterDeletedBy
    // metadata has Delete on HeaderHash
    todo!("write this test")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "write this test"]
async fn test_integrate_single_register_add_link() {
    // For RegisterAddLink
    // metadata has link on EntryHash
    todo!("write this test")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "write this test"]
async fn test_integrate_single_register_delete_link() {
    // For RegisterAddLink
    // metadata has link on EntryHash
    todo!("write this test")
}

// #[cfg(feature = "slow_tests")]
#[cfg(todo_redo_old_tests)]
mod slow_tests {
    use crate::test_utils::host_fn_caller::*;
    use crate::test_utils::setup_app;
    use crate::test_utils::wait_for_integration;
    use ::fixt::prelude::*;
    use fallible_iterator::FallibleIterator;
    use holo_hash::EntryHash;
    use holochain_serialized_bytes::SerializedBytes;
    use holochain_sqlite::prelude::*;
    use holochain_state::prelude::*;
    use holochain_types::prelude::*;
    use holochain_wasm_test_utils::TestWasm;
    use observability;
    use std::convert::TryFrom;
    use std::convert::TryInto;
    use std::time::Duration;
    use tracing::*;

    /// The aim of this test is to show from a high level that committing
    /// data on one agent results in integrated data on another agent
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "flaky"]
    async fn commit_entry_add_link() {
        //////////////
        //// Setup
        //////////////

        observability::test_run().ok();
        let dna_file = DnaFile::new(
            DnaDef {
                name: "integration_workflow_test".to_string(),
                uid: "ba1d046d-ce29-4778-914b-47e6010d2faf".to_string(),
                properties: SerializedBytes::try_from(()).unwrap(),
                zomes: vec![TestWasm::Create.into()].into(),
            },
            vec![TestWasm::Create.into()],
        )
        .await
        .unwrap();

        let alice_agent_id = fake_agent_pubkey_1();
        let alice_cell_id = CellId::new(dna_file.dna_hash().to_owned(), alice_agent_id.clone());
        let alice_installed_cell = InstalledCell::new(alice_cell_id.clone(), "alice_handle".into());

        let bob_agent_id = fake_agent_pubkey_2();
        let bob_cell_id = CellId::new(dna_file.dna_hash().to_owned(), bob_agent_id.clone());
        let bob_installed_cell = InstalledCell::new(bob_cell_id.clone(), "bob_handle".into());

        let (_tmpdir, _app_api, conductor) = setup_app(
            vec![(
                "test_app",
                vec![(alice_installed_cell, None), (bob_installed_cell, None)],
            )],
            vec![dna_file.clone()],
        )
        .await;

        //////////////
        //// The Test
        //////////////

        // Create the data to be committed
        let base = Post("Bananas are good for you".into());
        let target = Post("Potassium is radioactive".into());
        let base_entry = Entry::try_from(base.clone()).unwrap();
        let target_entry = Entry::try_from(target.clone()).unwrap();
        let base_entry_hash = EntryHash::with_data_sync(&base_entry);
        let target_entry_hash = EntryHash::with_data_sync(&target_entry);
        let link_tag = fixt!(LinkTag);

        // Commit the base and target.
        // Link them together.
        {
            let call_data = HostFnCaller::create(&alice_cell_id, &conductor, &dna_file).await;

            // 3
            call_data
                .commit_entry(base.clone().try_into().unwrap(), POST_ID)
                .await;

            // 4
            call_data
                .commit_entry(target.clone().try_into().unwrap(), POST_ID)
                .await;

            // 5
            // Link the entries
            call_data
                .create_link(
                    base_entry_hash.clone(),
                    target_entry_hash.clone(),
                    link_tag.clone(),
                )
                .await;

            // Produce and publish these commits
            let mut triggers = conductor.get_cell_triggers(&alice_cell_id).await.unwrap();
            triggers.produce_dht_ops.trigger();
        }

        // Check the ops
        {
            let call_data = HostFnCaller::create(&bob_cell_id, &conductor, &dna_file).await;

            // Wait for the ops to integrate but early exit if they do
            // 14 ops for genesis and 9 ops for two commits and a link
            // Try 100 times for 100 millis each so maximum wait is 10 seconds
            wait_for_integration(&call_data.env, 14 + 9, 100, Duration::from_millis(100)).await;

            // Check the ops are not empty
            let db = call_data
                .env
                .get_table(TableName::IntegratedDhtOps)
                .unwrap();
            let ops_db = IntegratedDhtOpsStore::new(call_data.env.clone(), db);

            fresh_reader_test!(call_data.env, |mut reader| {
                let ops = ops_db
                    .iter(&mut reader)
                    .unwrap()
                    .collect::<Vec<_>>()
                    .unwrap();
                debug!(?ops);
                assert!(!ops.is_empty());

                // Check the correct links is in bobs integrated metadata vault
                let meta = MetadataBuf::vault(call_data.env.clone().into()).unwrap();
                let key = LinkMetaKey::Base(&base_entry_hash);
                let links = meta
                    .get_live_links(&mut reader, &key)
                    .unwrap()
                    .collect::<Vec<_>>()
                    .unwrap();
                let link = links[0].clone();
                assert_eq!(link.target, target_entry_hash);
            });

            // Check bob can get the links
            let links = call_data
                .get_links(base_entry_hash.clone(), Some(link_tag), Default::default())
                .await;
            let link = links[0].clone();
            assert_eq!(link.target, target_entry_hash);

            // Check bob can get the target
            let e = call_data
                .get(target_entry_hash.clone().into(), GetOptions::content())
                .await
                .unwrap();
            assert_eq!(e.into_inner().1.into_option().unwrap(), target_entry);

            // Check bob can get the base
            let e = call_data
                .get(base_entry_hash.clone().into(), GetOptions::content())
                .await
                .unwrap();
            assert_eq!(e.into_inner().1.into_option().unwrap(), base_entry);
        }

        // Shut everything down
        let shutdown = conductor.take_shutdown_handle().await.unwrap();
        conductor.shutdown().await;
        shutdown.await.unwrap().unwrap();
    }
}
