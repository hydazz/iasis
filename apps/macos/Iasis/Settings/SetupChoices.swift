import Foundation

enum SetupChoices {
    struct Language: Identifiable {
        let id: String
        let name: String
    }

    struct Region: Identifiable {
        let id: String
        let name: String
        let locale: String
    }

    /// Language tags and one supported Setup Assistant locale per region.
    static let languages: [Language] = """
    ar bg bn ca cs da de el en en-AU en-CA en-GB en-IN en-PH en-US es es-419 es-US fi fr fr-CA gu he
    hi hr hu id it ja kk kk-Cyrl kn ko lt ml mr ms nb nl or pa pl pt-BR pt-PT ro ru sk sl sv ta te
    th tr uk ur vi zh-HK zh-Hans zh-Hant zh-Hant-HK
    """.split(whereSeparator: \.isWhitespace).map {
        let id = String($0)
        return Language(id: id, name: Locale.current.localizedString(forIdentifier: id) ?? id)
    }.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }

    static let regions: [Region] = """
    en_001 es_003 en_150 es_419 ca_AD ar_AE en_AG en_AI en_AL pt_AO es_AR en_AS de_AT en_AU nl_AW
    sv_AX hr_BA en_BB bn_BD nl_BE fr_BF bg_BG ar_BH en_BI fr_BJ fr_BL en_BM ms_BN es_BO es_BQ pt_BR
    en_BS en_BW ru_BY en_BZ en_CA en_CC fr_CD fr_CF fr_CG de_CH fr_CI en_CK es_CL fr_CM zh_CN es_CO
    es_CR es_CU pt_CV es_CW en_CX el_CY cs_CZ de_DE en_DG fr_DJ da_DK en_DM es_DO ar_DZ es_EA es_EC
    en_EE ar_EG ar_EH ar_ER es_ES fi_FI en_FJ en_FK en_FM fr_FR fr_GA en_GB en_GD en_GE fr_GF en_GG
    en_GH en_GI da_GL en_GM fr_GN fr_GP es_GQ el_GR en_GS es_GT en_GU pt_GW en_GY zh_HK es_HN hr_HR
    es_HT hu_HU es_IC id_ID en_IE he_IL en_IM hi_IN en_IO ar_IQ it_IT en_JE en_JM ar_JO ja_JP en_KE
    ru_KG en_KI ar_KM en_KN ko_KP ko_KR ar_KW en_KY ru_KZ ar_LB en_LC de_LI ta_LK en_LR en_LS lt_LT
    fr_LU en_LV ar_LY ar_MA fr_MC ro_MD fr_MF en_MG en_MH fr_ML en_MM zh_MO en_MP fr_MQ ar_MR en_MS
    en_MT fr_MU en_MV en_MW es_MX ms_MY pt_MZ en_NA fr_NC fr_NE en_NF en_NG es_NI nl_NL nb_NO en_NR
    en_NU en_NZ ar_OM es_PA es_PE fr_PF en_PG en_PH en_PK pl_PL fr_PM en_PN es_PR ar_PS pt_PT en_PW
    es_PY ar_QA fr_RE ro_RO ru_RU en_RW ar_SA en_SB fr_SC ar_SD sv_SE en_SG en_SH sl_SI nb_SJ sk_SK
    en_SL it_SM fr_SN ar_SO nl_SR ar_SS pt_ST es_SV en_SX ar_SY en_SZ en_TC ar_TD fr_TG th_TH en_TK
    pt_TL ar_TN en_TO tr_TR en_TT en_TV zh_TW en_TZ uk_UA en_UG en_UM en_US es_UY it_VA en_VC es_VE
    en_VG en_VI vi_VN en_VU fr_WF en_WS ar_YE fr_YT en_ZA en_ZM en_ZW
    """.split(whereSeparator: \.isWhitespace).compactMap {
        let locale = String($0)
        guard let id = Locale(identifier: locale).region?.identifier else { return nil }
        return Region(id: id, name: Locale.current.localizedString(forRegionCode: id) ?? id, locale: locale)
    }.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
}
