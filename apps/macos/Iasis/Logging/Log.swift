import OSLog

nonisolated enum Log {
    private static let subsystem = "com.hydaz.Iasis"

    static let agent = Logger(
        subsystem: subsystem,
        category: "agent"
    )
}
